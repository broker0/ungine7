//! Text-client — minimalist interactive TUI client for Ultima Online.
//!
//! Connects to a server, renders the world as ASCII, and provides
//! keyboard/mouse controls for movement, clicks, and speech.

mod app;
mod commands;
mod config;
mod game_session;
mod input;
mod movement;
mod ui;

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent,
    KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen,
    disable_raw_mode, enable_raw_mode,
};
use clap::Parser;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use files::radarcol::RadarColors;
use framework::diorama::ObserverEvent;
use framework::ecumene::StaticWorldData;
use network::client::{ClientConfig, PacketClient};
use network::session::SessionEvent;
use packets::traits::{ManualPacket, BasicPacket};
use packets::system::{ClientVersionResponse, GeneralInfo};
use protocol::RawPacket;
use protocol::connector::ConnectorConfig;
use protocol::packets::system::Ping;

use app::{App, AppScreen, ChatEntry, LoginState};
use game_session::GameSession;
use input::{heading_from_screen_delta, parse_mouse_event, MouseAction};

// ── Login result sent from the background task ───────────────────────────

struct LoginResult {
    session: GameSession,
    char_name: String,
    char_x: u16,
    char_y: u16,
    char_z: i8,
}

// ── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Parse CLI arguments ───────────────────────────────────────────
    let args = config::Args::parse();

    // ── Load static data ──────────────────────────────────────────────
    let static_data: Option<Arc<dyn framework::ecumene::StaticDataProvider>> =
        if let Some(data_dir) = args.data.path() {
            match StaticWorldData::load(data_dir) {
                Ok(sd) => {
                    eprintln!("Loaded world data from {}", data_dir.display());
                    Some(Arc::new(sd) as Arc<dyn framework::ecumene::StaticDataProvider>)
                }
                Err(e) => {
                    eprintln!("Warning: could not load world data from {}: {}", data_dir.display(), e);
                    eprintln!("         Map terrain will not be displayed.");
                    None
                }
            }
        } else {
            None
        };

    let radar_colors = args.data.path().and_then(|p| RadarColors::read(p).ok());

    // ── Set up TUI immediately (login screen first) ───────────────────
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let has_keyboard_enhancement = {
        use crossterm::event::{
            KeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        };
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
        )
        .is_ok()
    };

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // ── Build App (starts on login screen) ────────────────────────────
    let login_state = LoginState::new(
        args.server,
        args.client_version,
        args.encrypted,
    );
    let mut app = App::new(login_state);
    app.radar_colors = radar_colors;
    app.held_keys.has_release_events = has_keyboard_enhancement;

    // Channel for receiving login results from background task.
    let (login_tx, mut login_rx) = mpsc::channel::<Result<LoginResult, String>>(1);

    // ── Event loop ────────────────────────────────────────────────────
    let mut event_stream = crossterm::event::EventStream::new();
    let mut render_interval = tokio::time::interval(Duration::from_millis(66)); // ~15fps
    let mut move_interval = tokio::time::interval(Duration::from_millis(50));   // movement poll

    loop {
        if app.should_quit {
            break;
        }

        tokio::select! {
            // ── Login result from background task ─────────────────────
            Some(result) = login_rx.recv() => {
                match result {
                    Ok(lr) => {
                        app.login.connecting = false;
                        app.login.status = format!("Logged in as '{}'", lr.char_name);
                        app.push_chat(ChatEntry::system(format!(
                            "Connected as '{}' at ({},{},{})",
                            lr.char_name, lr.char_x, lr.char_y, lr.char_z,
                        )));
                        app.sessions.push(lr.session);
                        app.screen = AppScreen::Game;
                    }
                    Err(e) => {
                        app.login.connecting = false;
                        app.login.status = format!("Error: {}", e);
                    }
                }
            }

            // ── Network: S→C packets ──────────────────────────────────
            result = async {
                if app.screen == AppScreen::Game {
                    if let Some(session) = app.sessions.get_mut(0) {
                        if session.connected {
                            return Some(session.game.recv().await);
                        }
                    }
                }
                std::future::pending::<Option<network::session::RecvResult>>().await
            } => {
                if let Some(result) = result {
                    handle_network_event(&mut app, result);
                }
            }

            // ── Terminal events ───────────────────────────────────────
            Some(Ok(event)) = event_stream.next() => {
                handle_terminal_event(&mut app, event, &login_tx, &static_data).await;
            }

            // ── Render tick ───────────────────────────────────────────
            _ = render_interval.tick() => {
                if app.screen == AppScreen::Game {
                    app.map_area = ui::map_rect(terminal.get_frame().area());
                }
                terminal.draw(|frame| ui::render(frame, &app))?;
            }

            // ── Movement tick ─────────────────────────────────────────
            _ = move_interval.tick() => {
                if app.screen == AppScreen::Game && !app.input_mode {
                    app.held_keys.expire();

                    if let Some(heading) = app.held_keys.heading() {
                        let running = app.held_keys.running();
                        if let Some(session) = app.sessions.get_mut(app.active) {
                            let pkt = session.movement.try_step(
                                heading,
                                running,
                                &session.observer,
                                session.static_data.as_ref(),
                            );
                            if let Some(pkt) = pkt {
                                session.observer.ingest_c2s(&pkt.data);
                                let _ = session.game.send(pkt).await;
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Cleanup ───────────────────────────────────────────────────────
    disable_raw_mode()?;
    if has_keyboard_enhancement {
        use crossterm::event::PopKeyboardEnhancementFlags;
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen,
    )?;
    terminal.show_cursor()?;

    for session in &mut app.sessions {
        session.game.close().await;
    }

    eprintln!("Goodbye!");
    Ok(())
}

// ── Spawn login task ──────────────────────────────────────────────────────

fn spawn_login(
    address: String,
    account: String,
    password: String,
    server_index: u16,
    client_version: u_core::ProtocolVersion,
    encrypted: bool,
    static_data: Option<Arc<dyn framework::ecumene::StaticDataProvider>>,
    tx: mpsc::Sender<Result<LoginResult, String>>,
) {
    tokio::spawn(async move {
        let result = do_login(
            address, account, password, server_index,
            client_version, encrypted, static_data,
        ).await;
        let _ = tx.send(result.map_err(|e| e.to_string())).await;
    });
}

async fn do_login(
    address: String,
    account: String,
    password: String,
    server_index: u16,
    client_version: u_core::ProtocolVersion,
    encrypted: bool,
    static_data: Option<Arc<dyn framework::ecumene::StaticDataProvider>>,
) -> Result<LoginResult, Box<dyn std::error::Error + Send + Sync>> {
    let client = PacketClient::new(ClientConfig {
        version: client_version,
        encrypted,
        connector: ConnectorConfig::Direct,
    });

    let mut login = client.connect_login(&address, config::LOGIN_SEED).await?;
    login.authenticate(&account, &password).await?;
    let redirect = login.select_server(server_index).await?;

    let mut game = login.into_game(&redirect).await?;

    // Game-server login.
    game.authenticate(&account, &password).await?;
    let chars = game.receive_character_list().await?;
    let (slot, character) = chars.first_character()
        .ok_or("No characters found on account")?;
    let char_name = character.name.to_string();
    game.select_character(&char_name, slot as u32).await?;

    // Create observer early so it ingests every packet from the server
    // during world entry (items, mobiles, multis, position, etc.).
    let mut observer = framework::diorama::ObserverPipeline::new(static_data.clone());

    // Version string for 0xBD responses (e.g. "3.0.8" — without build).
    let ver = &client_version;
    let version_string = format!("{}.{}.{}", ver.major, ver.minor, ver.patch);

    // Wait for 0x55 LoginComplete, feeding every packet to the observer.
    let mut char_x: u16 = 0;
    let mut char_y: u16 = 0;
    let mut char_z: i8 = 0;
    loop {
        match game.recv().await.event {
            SessionEvent::Packet(p) => {
                observer.ingest_s2c(&p.data);
                let _ = observer.drain_events().count();

                match p.data.first().copied().unwrap_or(0) {
                    0x1B => {
                        if let Ok(body) = packets::character::CharacterLocaleAndBody::from_bytes(&p.data) {
                            char_x = body.x;
                            char_y = body.y;
                            char_z = body.z;
                        }
                    }
                    // Respond to ClientVersionRequest during login.
                    0xBD => {
                        let resp = ClientVersionResponse::new(&version_string);
                        let _ = game.send(RawPacket::c2s(resp.to_bytes())).await;
                    }
                    0x55 => break,
                    _ => {}
                }
            }
            SessionEvent::Disconnected => {
                return Err("Server disconnected during login".into());
            }
            SessionEvent::Error(e) => {
                return Err(format!("Network error during login: {}", e).into());
            }
            _ => {}
        }
    }

    // ── Post-login handshake packets (mimic real client) ──────────────
    //
    // Real UO clients send these immediately after 0x55 LoginComplete.
    // Without them, some servers will not process movement or speech.

    // 1. ClientVersionResponse (0xBD) — in case the server didn't ask yet.
    let resp = ClientVersionResponse::new(&version_string);
    let _ = game.send(RawPacket::c2s(resp.to_bytes())).await;

    // 2. ScreenSize (0xBF sub 0x0005).
    let screen_size = GeneralInfo::ScreenSize { unk1: 0, x: 1024, y: 768, unk2: 0 };
    let _ = game.send(RawPacket::c2s(screen_size.to_bytes())).await;

    // 3. ClientLanguage (0xBF sub 0x000B).
    let language = GeneralInfo::ClientLanguage {
        language: packets::u_io::FixedString::new("ENU"),
    };
    let _ = game.send(RawPacket::c2s(language.to_bytes())).await;

    let mut session = GameSession::new_with_observer(
        0, char_name.clone(), game, static_data, observer,
    );
    session.version_string = version_string;

    Ok(LoginResult {
        session,
        char_name,
        char_x,
        char_y,
        char_z,
    })
}

// ── Network event handler ─────────────────────────────────────────────────

fn handle_network_event(app: &mut App, result: network::session::RecvResult) {
    match result.event {
        SessionEvent::Packet(raw) => {
            let pkt_id = raw.data.first().copied().unwrap_or(0);

            if let Some(session) = app.sessions.get_mut(0) {
                session.observer.ingest_s2c(&raw.data);

                let events: Vec<ObserverEvent> = session.observer.drain_events().collect();
                for event in events {
                    app.handle_observer_event(event, 0);
                }
            }

            // Respond to ping.
            if pkt_id == 0x73 {
                if let Ok(ping) = Ping::from_bytes(&raw.data) {
                    if let Some(session) = app.sessions.get_mut(0) {
                        let pong = RawPacket::c2s(packets::traits::encode_packet(&ping));
                        session.pending_pong = Some(pong);
                    }
                }
            }

            // Respond to ClientVersionRequest (0xBD).
            if pkt_id == 0xBD {
                if let Some(session) = app.sessions.get_mut(0) {
                    if !session.version_string.is_empty() {
                        let resp = ClientVersionResponse::new(&session.version_string);
                        session.pending_replies.push(RawPacket::c2s(resp.to_bytes()));
                    }
                }
            }
        }
        SessionEvent::Disconnected => {
            app.push_chat(ChatEntry::system("Server disconnected."));
            if let Some(session) = app.sessions.get_mut(0) {
                session.connected = false;
            }
        }
        SessionEvent::Error(e) => {
            app.push_chat(ChatEntry::system(format!("Network error: {}", e)));
            if let Some(session) = app.sessions.get_mut(0) {
                session.connected = false;
            }
        }
        _ => {}
    }
}

// ── Terminal event handler ────────────────────────────────────────────────

async fn handle_terminal_event(
    app: &mut App,
    event: Event,
    login_tx: &mpsc::Sender<Result<LoginResult, String>>,
    static_data: &Option<Arc<dyn framework::ecumene::StaticDataProvider>>,
) {
    // Send any pending pong and queued reply packets.
    if let Some(session) = app.sessions.get_mut(0) {
        if let Some(pong) = session.pending_pong.take() {
            let _ = session.game.send(pong).await;
        }
        for reply in session.pending_replies.drain(..) {
            let _ = session.game.send(reply).await;
        }
    }

    match event {
        Event::Key(key) => {
            match app.screen {
                AppScreen::Login => handle_login_key(app, key, login_tx, static_data),
                AppScreen::Game  => handle_game_key(app, key).await,
            }
        }
        Event::Mouse(mouse) => {
            if app.screen == AppScreen::Game {
                handle_mouse_event(app, mouse).await;
            }
        }
        Event::Resize(_, _) => {}
        _ => {}
    }
}

// ── Login screen key handler ──────────────────────────────────────────────

fn handle_login_key(
    app: &mut App,
    key: KeyEvent,
    login_tx: &mpsc::Sender<Result<LoginResult, String>>,
    static_data: &Option<Arc<dyn framework::ecumene::StaticDataProvider>>,
) {
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return;
    }

    // Don't accept input while connecting.
    if app.login.connecting && key.code != KeyCode::Esc {
        return;
    }

    match key.code {
        KeyCode::Esc => {
            app.should_quit = true;
        }
        KeyCode::Tab => {
            app.login.focused = if key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) {
                app.login.focused.prev()
            } else {
                app.login.focused.next()
            };
        }
        KeyCode::BackTab => {
            app.login.focused = app.login.focused.prev();
        }
        KeyCode::Enter => {
            // Validate fields.
            if app.login.address.is_empty() {
                app.login.status = "Error: address is required".into();
                return;
            }
            if app.login.account.is_empty() {
                app.login.status = "Error: account is required".into();
                return;
            }

            app.login.connecting = true;
            app.login.status = format!("Connecting to {}...", app.login.address);

            spawn_login(
                app.login.address.clone(),
                app.login.account.clone(),
                app.login.password.clone(),
                app.login.server_index,
                app.login.client_version,
                app.login.encrypted,
                static_data.clone(),
                login_tx.clone(),
            );
        }
        KeyCode::Backspace => {
            let field = focused_field_mut(&mut app.login);
            field.pop();
        }
        KeyCode::Char(c) => {
            let field = focused_field_mut(&mut app.login);
            field.push(c);
        }
        _ => {}
    }
}

/// Get a mutable reference to the currently focused login field.
fn focused_field_mut(login: &mut app::LoginState) -> &mut String {
    match login.focused {
        app::LoginField::Address  => &mut login.address,
        app::LoginField::Account  => &mut login.account,
        app::LoginField::Password => &mut login.password,
    }
}

// ── Game screen key handler ───────────────────────────────────────────────

async fn handle_game_key(app: &mut App, key: KeyEvent) {
    app.held_keys.update_shift(key.modifiers);

    if app.input_mode {
        // ── Input mode: typing text ──────────────────────────────────
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return;
        }
        match key.code {
            KeyCode::Esc => {
                app.input_mode = false;
                app.input_buffer.clear();
            }
            KeyCode::Enter => {
                let text = app.input_buffer.clone();
                app.input_buffer.clear();
                app.input_mode = false;

                if !text.is_empty() {
                    let cmd = commands::parse(&text);

                    match &cmd {
                        commands::Command::Quit => {
                            app.should_quit = true;
                            return;
                        }
                        commands::Command::Help => {
                            app.push_chat(ChatEntry::system("Commands: .say .yell .whisper .emote .click .dclick .attack .status .pos .who .quit .help"));
                            app.push_chat(ChatEntry::system("Type without . prefix to say in normal speech."));
                            return;
                        }
                        commands::Command::Pos => {
                            if let Some(session) = app.active_session() {
                                let (x, y, z) = session.position();
                                let dir = session.observer.pos.facing.heading();
                                app.push_chat(ChatEntry::system(
                                    format!("Position: {},{},{} facing {}", x, y, z, dir),
                                ));
                            }
                            return;
                        }
                        commands::Command::Who => {
                            let entries: Vec<ChatEntry> = if let Some(session) = app.active_session() {
                                let (px, py, _) = session.position();
                                let my_serial = session.serial();
                                session.observer.session.visible.iter()
                                    .filter(|e| e.is_mobile() && e.serial != my_serial)
                                    .map(|entity| {
                                        let dist = ((entity.x() as i32 - px as i32).unsigned_abs())
                                            .max((entity.y() as i32 - py as i32).unsigned_abs());
                                        ChatEntry::world(format!(
                                            "Mobile {:#010X} body={:#06X} at ({},{},{}) dist={}",
                                            entity.serial,
                                            entity.graphic(),
                                            entity.x(), entity.y(), entity.z(),
                                            dist,
                                        ))
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            };
                            let count = entries.len();
                            for entry in entries {
                                app.push_chat(entry);
                            }
                            app.push_chat(ChatEntry::system(format!("{} mobile(s) in view", count)));
                            return;
                        }
                        commands::Command::Unknown(msg) => {
                            if !msg.is_empty() {
                                app.push_chat(ChatEntry::system(msg.clone()));
                            }
                            return;
                        }
                        _ => {}
                    }

                    if let Some(pkt) = commands::build_packet(&cmd) {
                        if let Some(session) = app.sessions.get_mut(app.active) {
                            let _ = session.game.send(pkt).await;
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                app.input_buffer.pop();
            }
            KeyCode::Char(c) => {
                app.input_buffer.push(c);
            }
            _ => {}
        }
    } else {
        // ── Game mode: movement and shortcuts ────────────────────────
        if app.held_keys.handle_key(&key) {
            return;
        }

        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            KeyCode::Enter => {
                app.input_mode = true;
            }
            KeyCode::Esc => {
                app.should_quit = true;
            }
            KeyCode::PageUp => {
                app.chat_scroll = app.chat_scroll.saturating_add(3);
                let max = app.chat_log.len().saturating_sub(1);
                app.chat_scroll = app.chat_scroll.min(max);
            }
            KeyCode::PageDown => {
                app.chat_scroll = app.chat_scroll.saturating_sub(3);
            }
            _ => {}
        }
    }
}

// ── Mouse handler (game screen only) ──────────────────────────────────────

async fn handle_mouse_event(app: &mut App, mouse: crossterm::event::MouseEvent) {
    let Some(action) = parse_mouse_event(&mouse) else {
        return;
    };

    match action {
        MouseAction::LeftClick { col, row } => {
            let now = Instant::now();
            let is_double = app.last_click
                .map(|(t, c, r)| {
                    now.duration_since(t) < Duration::from_millis(400) && c == col && r == row
                })
                .unwrap_or(false);

            if is_double {
                app.last_click = None;
                if let Some((wx, wy)) = app.screen_to_world(col, row) {
                    if let Some(serial) = app.entity_at(wx, wy) {
                        let pkt = packets::interaction::DoubleClick {
                            id: packets::interaction::DoubleClick::ID,
                            serial,
                        };
                        if let Some(session) = app.sessions.get_mut(app.active) {
                            let _ = session.game.send(RawPacket::c2s(pkt.to_bytes())).await;
                        }
                        app.push_chat(ChatEntry::system(format!("Double-click {:#010X}", serial)));
                    }
                }
            } else {
                app.last_click = Some((now, col, row));
                if let Some((wx, wy)) = app.screen_to_world(col, row) {
                    if let Some(serial) = app.entity_at(wx, wy) {
                        let pkt = packets::interaction::SingleClick {
                            id: packets::interaction::SingleClick::ID,
                            serial,
                        };
                        if let Some(session) = app.sessions.get_mut(app.active) {
                            let _ = session.game.send(RawPacket::c2s(pkt.to_bytes())).await;
                        }
                    }
                }
            }
        }
        MouseAction::RightClick { col, row } | MouseAction::RightHeld { col, row } => {
            let heading = {
                let area = app.map_area;
                let inner_x = area.x + 1;
                let inner_y = area.y + 1;
                let inner_w = area.width.saturating_sub(2) as i32;
                let inner_h = area.height.saturating_sub(2) as i32;
                let center_x = inner_x as i32 + inner_w / 2;
                let center_y = inner_y as i32 + inner_h / 2;

                let dx = col as i32 - center_x;
                let dy = row as i32 - center_y;
                heading_from_screen_delta(dx, dy)
            };

            if let Some(heading) = heading {
                let running = app.held_keys.running();
                if let Some(session) = app.sessions.get_mut(app.active) {
                    let pkt = session.movement.try_step(
                        heading,
                        running,
                        &session.observer,
                        session.static_data.as_ref(),
                    );
                    if let Some(pkt) = pkt {
                        session.observer.ingest_c2s(&pkt.data);
                        let _ = session.game.send(pkt).await;
                    }
                }
            }
        }
        _ => {}
    }
}

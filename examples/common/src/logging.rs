use log::LevelFilter;

/// Convenience entry point — returns a [`LoggerBuilder`] with sensible defaults.
///
/// # Examples
///
/// ```no_run
/// use common::logging::init_logger;
/// use log::LevelFilter;
///
/// // All defaults: Debug level, no filters
/// init_logger().build().unwrap();
///
/// // Custom level + target filter
/// init_logger()
///     .level(LevelFilter::Trace)
///     .filter("protocol::session")
///     .build()
///     .unwrap();
///
/// // Per-target level overrides
/// init_logger()
///     .level(LevelFilter::Warn)                              // default: Warn
///     .targets(["my_app", "my_lib"], LevelFilter::Debug)     // these: Debug
///     .targets(["protocol::transport::raw"], LevelFilter::Trace)
///     .build()
///     .unwrap();
///
/// // Preset: only protocol-level logs
/// init_logger().protocol_only().build().unwrap();
/// ```
pub fn init_logger() -> LoggerBuilder {
    LoggerBuilder::default()
}

/// Builder for configuring the terminal logger used by examples.
///
/// Wraps [`fern`] so every example doesn't have to repeat the same
/// boilerplate.  Supports:
///
/// - Global log level (`.level()`, `.debug()`, `.verbose()`, `.quiet()`)
/// - Allow-list target filters (`.filter()`, `.filters()`)
/// - Per-target level overrides (`.level_for()`)
/// - Coloured output on all platforms including Windows
pub struct LoggerBuilder {
    level: LevelFilter,
    filters: Vec<String>,
    level_overrides: Vec<(String, LevelFilter)>,
}

impl Default for LoggerBuilder {
    fn default() -> Self {
        Self {
            level: LevelFilter::Debug,
            filters: vec![],
            level_overrides: vec![],
        }
    }
}

impl LoggerBuilder {
    /// Set the maximum log level (default: `Debug`).
    pub fn level(mut self, level: LevelFilter) -> Self {
        self.level = level;
        self
    }

    /// Add a single target allow-filter.
    ///
    /// When at least one filter is added, only log records whose target
    /// starts with one of the filter strings will be printed.
    /// Without any filters all targets are shown.
    pub fn filter(mut self, target: impl Into<String>) -> Self {
        self.filters.push(target.into());
        self
    }

    /// Add multiple target allow-filters at once.
    pub fn filters<I, S>(mut self, targets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.filters.extend(targets.into_iter().map(Into::into));
        self
    }

    /// Set the log level for a specific target, overriding the global level.
    ///
    /// Can be called multiple times for different targets.
    pub fn level_for(mut self, target: impl Into<String>, level: LevelFilter) -> Self {
        self.level_overrides.push((target.into(), level));
        self
    }

    /// Set the log level for multiple targets at once.
    ///
    /// ```no_run
    /// # use common::logging::init_logger;
    /// # use log::LevelFilter;
    /// init_logger()
    ///     .level(LevelFilter::Warn)
    ///     .targets(["my_app", "my_lib"], LevelFilter::Debug)
    ///     .targets(["protocol::transport::raw"], LevelFilter::Trace)
    ///     .build()
    ///     .unwrap();
    /// ```
    pub fn targets<I, S>(mut self, targets: I, level: LevelFilter) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for target in targets {
            self.level_overrides.push((target.into(), level));
        }
        self
    }

    // ── Presets ────────────────────────────────────────────────────────

    /// Only show logs from the `protocol` library
    /// (sessions, binder, detection, transport, pipeline, etc.).
    ///
    /// Useful for debugging protocol-level issues without noise
    /// from application code or the framework.
    pub fn protocol_only(self) -> Self {
        self.filter("protocol")
    }

    /// Show filter, binder, and redirect logs (from both protocol and framework).
    pub fn default_filter(self) -> Self {
        self.filters([
            "protocol::binder",
            "network::filter",
            "network::redirect",
        ])
    }

    /// Trace-level logging with no filters — maximum verbosity.
    pub fn verbose(self) -> Self {
        self.level(LevelFilter::Trace)
    }

    /// Debug-level logging — medium verbosity.
    pub fn debug(self) -> Self {
        self.level(LevelFilter::Debug)
    }

    /// Info-level logging — suppress debug/trace noise.
    pub fn quiet(self) -> Self {
        self.level(LevelFilter::Info)
    }

    /// Initialize the global logger.
    ///
    /// Must be called exactly once per process — subsequent calls will
    /// return an error (standard `log` crate behaviour).
    pub fn build(self) -> Result<(), log::SetLoggerError> {
        use colored::Colorize;
        use std::time::{SystemTime, UNIX_EPOCH};

        // Enable ANSI escape code processing on Windows so that
        // `colored` output renders correctly in cmd.exe / PowerShell.
        #[cfg(windows)]
        { colored::control::set_virtual_terminal(true).ok(); }

        let mut dispatch = fern::Dispatch::new()
            .format(|out, message, record| {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                let total_secs = now.as_secs();
                let millis = now.subsec_millis();
                let h = (total_secs / 3600) % 24;
                let m = (total_secs / 60) % 60;
                let s = total_secs % 60;

                let level_str = match record.level() {
                    log::Level::Error => "ERROR".red().bold(),
                    log::Level::Warn  => "WARN ".yellow(),
                    log::Level::Info  => "INFO ".green(),
                    log::Level::Debug => "DEBUG".blue(),
                    log::Level::Trace => "TRACE".dimmed(),
                };

                out.finish(format_args!(
                    "{h:02}:{m:02}:{s:02}.{millis:03} [{level_str}] {message}"
                ))
            });

        // ── Allow-list filters ────────────────────────────────────────
        // When filters are present, we implement them as a custom filter
        // function that checks whether the record's target starts with
        // any of the allowed prefixes.
        if !self.filters.is_empty() {
            let allowed = self.filters;
            dispatch = dispatch.filter(move |meta| {
                let target = meta.target();
                allowed.iter().any(|prefix| target.starts_with(prefix.as_str()))
            });
        }

        // ── Level configuration ───────────────────────────────────────
        dispatch = dispatch.level(self.level);
        for (target, level) in self.level_overrides {
            dispatch = dispatch.level_for(target, level);
        }

        dispatch
            .chain(std::io::stdout())
            .apply()
    }
}

use axum::{routing::{get, post}, Router, Json, response::{Html, Response}};
use axum::body::{Body, Bytes};
use axum::extract::State;
use serde_json::Value;
use std::sync::Arc;
use crate::state::AppState;

mod world;
mod mirror;

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/",        post(api_handler))
        .route("/api",         post(api_handler))
        .route("/api/render/", post(render_handler))
        .route("/ws/mirror",   get(mirror::ws_mirror_handler))
        .route("/ui.html",     get(serve_ui))
        .route("/",            get(serve_ui))
        .route("/ui",          get(serve_ui))
        .with_state(state)
}

async fn serve_ui() -> Html<String> {
    const EMBEDDED: &str = include_str!("../../www/ui.html");
    Html(EMBEDDED.to_string())
}

/// JSON API — accepts any Content-Type and parses body as JSON.
///
/// Stealth/Orion `HTTP_Post` may not send `Content-Type: application/json`,
/// so we read raw bytes and parse manually instead of using axum's `Json`
/// extractor (which rejects non-JSON content types with 415).
async fn api_handler(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Json<Value> {
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let preview: String = String::from_utf8_lossy(&body[..body.len().min(200)]).into();
            log::warn!("[http] failed to parse JSON: {e}. Body preview: {preview}");
            return Json(serde_json::json!({"Error": format!("Invalid JSON: {e}")}));
        }
    };
    world::handle_request(state, payload).await
}

/// Binary PNG endpoint: RenderArea only
async fn render_handler(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Response<Body> {
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Response::builder()
                .status(axum::http::StatusCode::BAD_REQUEST)
                .body(Body::from(format!("Invalid JSON: {e}")))
                .unwrap();
        }
    };
    crate::render::handle_render_area_binary(state, payload).await
}

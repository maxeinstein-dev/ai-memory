//! Route module — assembles the public axum router.

use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;

use crate::state::WebState;
use crate::templates::NotFoundView;

mod api;
mod index;
mod page;
mod project;
mod search;
mod statics;

/// Render a 404 response with the not-found template body. Shared by every
/// HTML route in this crate (moved here from `page.rs` in Tarefa 2 so the
/// tela do painel introduced in Tarefa 3 can reuse it).
pub(crate) fn not_found_response() -> Response {
    let html = NotFoundView {}
        .render()
        .unwrap_or_else(|_| "<h1>Not found</h1>".to_owned());
    (StatusCode::NOT_FOUND, Html(html)).into_response()
}

/// Build the read-only web router from a shared [`WebState`].
pub(crate) fn build(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/", get(index::handler))
        .route("/w/{workspace}/{project}", get(project::handler))
        .route("/w/{workspace}/{project}/p/{*path}", get(page::handler))
        .route("/search", get(search::handler))
        .route("/static/tailwind.css", get(statics::tailwind_css))
        .route("/static/logo.png", get(statics::logo))
        .with_state(state)
}

/// Build the read-only JSON API router from a shared [`WebState`].
pub(crate) fn build_api(state: Arc<WebState>) -> Router {
    api::build(state)
}

/// Standalone `GET /favicon.ico` router. Mounted at the **host root**
/// by `serve`, OUTSIDE the `/web` nest and OUTSIDE the `--base-path`
/// prefix, because browsers auto-fetch `/favicon.ico` from the host
/// origin regardless of where the rest of the app is mounted. Putting
/// it inside the web router (as the original PR #79 did) made it
/// reachable only at `/web/favicon.ico` — never seen by the browser's
/// automatic fetch — so the in-page `<link rel="icon">` was the only
/// thing actually showing the icon.
pub(crate) fn build_favicon() -> Router {
    Router::new().route("/favicon.ico", get(statics::favicon))
}

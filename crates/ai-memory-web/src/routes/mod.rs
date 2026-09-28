//! Route module — assembles the public axum router.

use std::sync::Arc;

use ai_memory_core::{ProjectId, WorkspaceId};
use ai_memory_store::{ResolvedScope, lookup_existing_scope};
use askama::Template;
use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::state::WebState;
use crate::templates::NotFoundView;

pub(crate) mod api;
pub(crate) mod handoff_web;
mod index;
mod page;
pub(crate) mod painel_briefing;
pub(crate) mod painel_cross_project;
pub(crate) mod painel_overview;
pub(crate) mod painel_proposals;
pub(crate) mod painel_timeline;
mod project;
mod search;
mod statics;

/// Render a 404 response with the not-found template body. Shared by every
/// HTML route in this crate.
pub(crate) fn not_found_response() -> Response {
    let html = NotFoundView {}
        .render()
        .unwrap_or_else(|_| "<h1>Not found</h1>".to_owned());
    (StatusCode::NOT_FOUND, Html(html)).into_response()
}

/// Resolve the scope of a panel screen without creating anything: an unknown
/// workspace/project answers the HTML 404 page, any other store failure a
/// logged 500. Unlike the Páginas tab's page tree (`project::handler`, which
/// renders an empty tree for an unknown project instead), panel screens need
/// real ids to query with.
pub(crate) async fn escopo_html(
    state: &WebState,
    workspace: &str,
    project: &str,
) -> Result<(WorkspaceId, ProjectId), Response> {
    lookup_existing_scope(&state.reader, workspace, project)
        .await
        .map(ResolvedScope::as_tuple)
        .map_err(|err| {
            if err.is_not_found() {
                not_found_response()
            } else {
                tracing::error!(error = %err, "resolvendo escopo da tela do painel");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        })
}

/// `X-Frame-Options` header name. Not a standard header in the `http` crate
/// (it's legacy/non-standard, unlike `Content-Security-Policy`), so it has
/// no `axum::http::header` constant — build it explicitly.
static X_FRAME_OPTIONS: header::HeaderName = header::HeaderName::from_static("x-frame-options");

/// Build the read-only web router from a shared [`WebState`].
///
/// Every response from this router carries `X-Frame-Options: DENY` and
/// `Content-Security-Policy: frame-ancestors 'none'` (redundant on purpose —
/// the CSP directive is what modern browsers honour, `X-Frame-Options` is
/// the fallback for the rest): this is a locally-bound, unauthenticated-by-
/// default HTML surface (`/web`), so nothing here should ever be embeddable
/// in another page's `<iframe>`/`<frame>`/`<object>`, which is what makes a
/// clickjacking attack against `POST /handoff` (dressing this panel up
/// behind an invisible frame on a malicious page) possible in the first
/// place. Applied once, here, so it covers every route in this router
/// (including future ones) rather than being bolted onto `/handoff` alone.
pub(crate) fn build(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/", get(index::handler))
        .route("/w/{workspace}/{project}", get(painel_overview::handler))
        .route("/w/{workspace}/{project}/paginas", get(project::handler))
        .route("/w/{workspace}/{project}/p/{*path}", get(page::handler))
        .route(
            "/w/{workspace}/{project}/briefing",
            get(painel_briefing::handler),
        )
        .route(
            "/w/{workspace}/{project}/linha-do-tempo",
            get(painel_timeline::handler),
        )
        .route(
            "/w/{workspace}/{project}/propostas",
            get(painel_proposals::handler),
        )
        .route("/entre-projetos", get(painel_cross_project::handler))
        // The panel's first write route — see
        // docs/alfama/specs/2026-09-28-handoff-pela-tela.md. NOT mounted on
        // `build_api` below: only the built-in server-rendered browser
        // exposes it, never the JSON API.
        .route("/handoff", post(handoff_web::handler))
        .route("/search", get(search::handler))
        .route("/static/tailwind.css", get(statics::tailwind_css))
        .route("/static/logo.png", get(statics::logo))
        .layer(SetResponseHeaderLayer::overriding(
            X_FRAME_OPTIONS.clone(),
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'none'"),
        ))
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

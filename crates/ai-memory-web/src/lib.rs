//! `ai-memory-web` — HTTP browser for the wiki.
//!
//! Mounted under `/web` on the same axum server that hosts the MCP
//! endpoint, so a single port + single auth posture covers both. The
//! crate was read-only through v1 (upstream still documents it that way:
//! "no editing, no POST routes, no agent-write APIs" — see
//! `docs/security-boundaries.md`). This fork adds exactly one write route,
//! `POST /handoff` (see `docs/alfama/specs/2026-09-28-handoff-pela-tela.md`);
//! every other route stays read-only by construction. The wiki is already
//! markdown-on-disk; this surface just makes it browsable from a phone, a
//! tablet, or a teammate's machine without `docker exec cat …`.
//!
//! Routes (all under whatever prefix the host nests this router at):
//! - `GET /`                                → project list (cards)
//! - `GET /w/:workspace/:project`           → project overview (Visão geral)
//! - `GET /w/:workspace/:project/paginas`   → page tree + recent activity
//! - `GET /w/:workspace/:project/p/*path`   → rendered markdown + metadata
//! - `GET /w/:workspace/:project/briefing`  → session-start brief preview
//! - `GET /search?q=…`                      → FTS5 hit list
//! - `GET /static/*`                        → embedded CSS + logo
//! - `POST /handoff`                        → fork-only: send a page as a
//!   handoff to another project, CSRF-protected (see `crate::csrf`)
//!
//! The companion `api_router` exposes the same read-only data as JSON
//! for custom frontends. It intentionally does not expose write/admin
//! operations — `POST /handoff` is not mounted there, only on the
//! built-in server-rendered browser.
//!
//! Theme follows `prefers-color-scheme` via the included Tailwind
//! stylesheet; no JS toggle, no cookie.

use std::sync::Arc;

use ai_memory_core::{ProjectId, WorkspaceId};
use ai_memory_store::ReaderPool;
use ai_memory_wiki::Wiki;
use axum::Router;

mod csrf;
mod markdown;
pub mod mount;
mod routes;
mod state;
mod templates;

pub use mount::{
    SplitWebRouters, WebMountSpec, inject_base_href, inject_base_path_meta, normalize_prefix,
    split_web_routers, web_base_href,
};
pub use state::WebState;

/// Build the read-only web router. Call once at server startup and
/// `nest("/web", router)` it onto the existing axum app, OR mount at
/// `/` if the web UI is the only HTTP surface.
pub fn router(reader: ReaderPool, wiki: Wiki) -> Router {
    let state = Arc::new(WebState::new(reader, wiki));
    routes::build(state)
}

/// Build the read-only JSON API router for third-party web UIs.
///
/// The host should `nest("/api/v1", api_router(...))` alongside `/web`
/// so custom frontends can browse memory without reading SQLite or wiki
/// files directly.
pub fn api_router(reader: ReaderPool, wiki: Wiki) -> Router {
    let state = Arc::new(WebState::new(reader, wiki));
    routes::build_api(state)
}

/// Standalone `GET /favicon.ico` router. Merge at the host-root level
/// (NOT under `--base-path`, NOT nested under `/web`) so the browser's
/// automatic `/favicon.ico` fetch actually reaches it. The handler is
/// stateless — it returns the same embedded PNG as `/web/static/logo.png`.
pub fn favicon_router() -> Router {
    routes::build_favicon()
}

/// Test-only entry point to the briefing screen's data: the suite needs the
/// raw markdown, which the rendered HTML escapes. Not a supported API.
#[doc(hidden)]
pub async fn montar_briefing_para_teste(
    reader: &ReaderPool,
    ws: WorkspaceId,
    proj: ProjectId,
    max_chars: Option<&str>,
) -> anyhow::Result<routes::painel_briefing::Briefing> {
    routes::painel_briefing::montar(reader, ws, proj, max_chars).await
}

/// Test-only entry point: build the built-in browser router (the one
/// `POST /handoff` is mounted on) with a caller-supplied CSRF key instead of
/// a freshly generated random one. [`router`] never exposes its internal
/// [`WebState`], and every real key is random by construction (`CsrfKey::
/// generate`), so the adversarial CSRF suite (wrong page, expired token,
/// wrong key) has no way to precompute a token against the router under
/// test without this — the same shape as `montar_briefing_para_teste`
/// above. Not a supported API.
#[doc(hidden)]
pub fn router_with_csrf_key_for_test(reader: ReaderPool, wiki: Wiki, key: [u8; 32]) -> Router {
    let mut state = WebState::new(reader, wiki);
    state.csrf_key = Arc::new(csrf::CsrfKey::from_bytes(key));
    routes::build(Arc::new(state))
}

/// Test-only: issue a CSRF token exactly like `page.html` does, for a
/// caller-chosen key/scope/minute — the counterpart to
/// [`router_with_csrf_key_for_test`]. Not a supported API.
#[doc(hidden)]
#[must_use]
pub fn csrf_token_for_test(
    key: [u8; 32],
    from_workspace: &str,
    from_project: &str,
    from_path: &str,
    minute: i64,
) -> String {
    csrf::issue(
        &csrf::CsrfKey::from_bytes(key),
        from_workspace,
        from_project,
        from_path,
        minute,
    )
}

/// Test-only: `crate::csrf::now_unix_minute`, for computing token minute
/// offsets from the suite. Not a supported API.
#[doc(hidden)]
#[must_use]
pub fn now_unix_minute_for_test() -> i64 {
    csrf::now_unix_minute()
}

/// Test-only: exercise `POST /handoff`'s scope-resolution + write logic
/// directly, bypassing the CSRF/Origin gate entirely. Exists for exactly one
/// guarantee that must hold independently of CSRF verification (see
/// `docs/alfama/plans/2026-09-28-handoff-pela-tela.md` Tarefa 3 Passo 5): a
/// forged `from_workspace`/`from_project` must not change which project a
/// handoff lands in. Not a supported API.
#[cfg(test)]
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn create_handoff_for_test(
    reader: &ReaderPool,
    wiki: &Wiki,
    from_workspace: &str,
    from_project: &str,
    from_path: &str,
    to_workspace: &str,
    to_project: &str,
    summary: &str,
) -> Result<(), String> {
    let state = WebState::new(reader.clone(), wiki.clone());
    let form = routes::handoff_web::HandoffForm::for_test(
        from_workspace,
        from_project,
        from_path,
        to_workspace,
        to_project,
        summary,
    );
    routes::handoff_web::create_handoff(&state, &form)
        .await
        .map_err(|e| format!("{e:?}"))
}

// Integration tests compile into this crate's test harness instead of a
// separate binary: every test binary is another link and, on macOS and
// Windows, another first-run malware scan. They still exercise only the
// public API; `extern crate self` lets them keep addressing it by crate name.
#[cfg(test)]
extern crate self as ai_memory_web;
#[cfg(test)]
#[path = "../tests/suite/mod.rs"]
mod integration;

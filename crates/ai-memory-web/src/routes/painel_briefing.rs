//! `GET /w/:workspace/:project/briefing` — the exact text the session-start
//! hook injects for this project, with how much of the char budget it uses.

use std::sync::Arc;

use ai_memory_core::{ProjectId, SlotVisibility, WorkspaceId};
use ai_memory_store::ReaderPool;
use ai_memory_store::brief;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::markdown;
use crate::routes::escopo_html;
use crate::state::WebState;
use crate::templates::{BriefingView, ItemDoBriefing, project_href};

/// Query string: `?max_chars=` simulates another `[briefing] max_chars`.
/// Kept as a raw string so an unparseable value falls back exactly the way
/// the hook's `briefing_budget` does, inside [`brief::clamp_brief_budget`].
#[derive(Deserialize)]
pub(crate) struct Params {
    max_chars: Option<String>,
}

/// What the briefing screen shows, before HTML rendering.
pub struct Briefing {
    /// The brief markdown, byte-for-byte what the hook would inject; empty
    /// when the project has no page the brief would carry.
    pub markdown: String,
    /// Effective byte budget after clamping.
    pub(crate) orcamento: usize,
    /// Core pages the store returned, whether or not they fit the budget.
    pub(crate) itens: Vec<ItemDoBriefing>,
}

/// Exact header the renderer gives a core page's body (`render_session_brief`
/// in `brief.rs`): `` (`path`) `` immediately followed by a newline. Unlike
/// the omitted section's `` - `path` `` line or the recent section's ``
/// (`path`, kind, timestamp) `` line, this only ever appears when the page's
/// own body was rendered — so it is what "entered the briefing" actually
/// means, unlike matching on the page title (which can appear incidentally
/// inside another page's body or the recent-pointers section).
fn corpo_header_marker(path: &str) -> String {
    format!("(`{path}`)\n")
}

/// Same pages, same renderer and same clamp as the hook — both now go
/// through [`ai_memory_store::brief::build_session_brief`], so this preview
/// cannot drift from what the hook injects.
///
/// `SlotVisibility::All`: the web already lists every page of the project
/// (invariant 16), so with `[slots] per_user = true` the preview shows the
/// union of every user's slots — and the screen says so.
pub(crate) async fn montar(
    reader: &ReaderPool,
    ws: WorkspaceId,
    proj: ProjectId,
    max_chars: Option<&str>,
) -> anyhow::Result<Briefing> {
    let (markdown, orcamento, core) =
        brief::build_session_brief(reader, ws, proj, SlotVisibility::All, max_chars).await?;
    let markdown = markdown.unwrap_or_default();
    let itens = core
        .iter()
        .map(|p| ItemDoBriefing {
            path: p.path.clone(),
            title: p.title.clone(),
            chars: p.body.trim().len(),
            entrou: markdown.contains(&corpo_header_marker(&p.path)),
        })
        .collect();
    Ok(Briefing {
        markdown,
        orcamento,
        itens,
    })
}

/// Handler for `GET /w/:workspace/:project/briefing`.
pub(crate) async fn handler(
    State(state): State<Arc<WebState>>,
    Path((workspace, project)): Path<(String, String)>,
    Query(params): Query<Params>,
) -> Response {
    let (ws, proj) = match escopo_html(&state, &workspace, &project).await {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    let b = match montar(&state.reader, ws, proj, params.max_chars.as_deref()).await {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(error = %err, "building briefing");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let usados = b.markdown.len();
    let html = markdown::render(&b.markdown, &workspace, &project);
    let view = BriefingView {
        base_href: project_href(&workspace, &project),
        aba: "briefing",
        usados,
        orcamento: b.orcamento,
        pct: (usados * 100 / b.orcamento.max(1)).min(100),
        orcamento_padrao: brief::BRIEF_BUDGET_DEFAULT,
        html,
        markdown: b.markdown,
        itens: b.itens,
        workspace,
        project,
    };
    match view.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(error = %err, "rendering briefing");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

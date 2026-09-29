//! `GET /w/:workspace/:project/p/*path` — rendered markdown page.

use std::sync::Arc;

use ai_memory_core::PagePath;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::markdown;
use crate::routes::not_found_response;
use crate::state::WebState;
use crate::templates::{NamespaceView, PageRow, PageView, humanize, page_href, project_href};

/// `?handoff=enviado` / `?handoff=erro&motivo=...` set by `POST /handoff`'s
/// redirect (routes::handoff_web) so this GET can render a one-time notice.
/// Not itself security-relevant: it only controls which banner is shown,
/// never who can write.
#[derive(Debug, Deserialize)]
pub(crate) struct HandoffNotice {
    handoff: Option<String>,
    #[serde(default)]
    motivo: Option<String>,
}

/// Handler for `GET /w/:workspace/:project/p/*path`.
pub(crate) async fn handler(
    State(state): State<Arc<WebState>>,
    Path((workspace, project, path)): Path<(String, String, String)>,
    Query(notice): Query<HandoffNotice>,
) -> Response {
    let meta = match state.reader.page_meta(&workspace, &project, &path).await {
        Ok(Some(m)) => m,
        // Not a page — it may be a namespace (directory) link, e.g. the OKF
        // bundle index's `[_lint/](_lint/)`. Render a listing rather than a
        // bare 404 (#603).
        Ok(None) => return namespace_or_not_found(&state, &workspace, &project, &path).await,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let page_path = match PagePath::new(&path) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    let markdown_doc = match state
        .wiki
        .read_page(meta.workspace_id, meta.project_id, &page_path)
    {
        Ok(doc) => doc,
        Err(_) => return not_found_response(),
    };

    // Drop a leading H1 that repeats the title — the template already
    // renders the title in its header, so leaving it in the body
    // duplicates it.
    let body_html = markdown::render(
        markdown::strip_leading_h1(&markdown_doc.body, &meta.title),
        &workspace,
        &project,
    );

    let project_href = project_href(&workspace, &project);
    let supersedes_path = meta.supersedes.unwrap_or_default();
    let supersedes_href = if supersedes_path.is_empty() {
        String::new()
    } else {
        page_href(&workspace, &project, &supersedes_path)
    };

    let (author_username, author_name, author_email) = meta.author.map_or_else(
        || (String::new(), String::new(), String::new()),
        |a| {
            (
                a.username,
                a.name.unwrap_or_default(),
                a.email.unwrap_or_default(),
            )
        },
    );

    // CSRF token scoped to exactly this page's own (workspace, project,
    // path) — never anything the "Enviar como handoff" form itself could
    // supply. Signs `meta.path` (not the raw URL `path` extractor value)
    // because that is exactly what the hidden `from_path` field below
    // renders as `PageView.path` — they must be byte-identical or a
    // legitimately submitted form would fail its own CSRF check. See
    // crate::csrf and POST /handoff (routes::handoff_web).
    let csrf_token = crate::csrf::issue(
        &state.csrf_key,
        &workspace,
        &project,
        &meta.path,
        crate::csrf::now_unix_minute(),
    );
    let known_projects = state
        .reader
        .list_projects_with_stats()
        .await
        .map(|summaries| {
            summaries
                .into_iter()
                .map(|s| s.project_name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    match (PageView {
        workspace,
        project,
        project_href,
        path: meta.path,
        title: meta.title,
        kind: meta.kind,
        tier: meta.tier,
        pinned: meta.pinned,
        updated_relative: humanize(&meta.updated_at),
        created_relative: humanize(&meta.created_at),
        supersedes_path,
        supersedes_href,
        body_html,
        author_username,
        author_name,
        author_email,
        csrf_token,
        known_projects,
        handoff_status: notice.handoff.unwrap_or_default(),
        handoff_motivo: notice.motivo.unwrap_or_default(),
    }
    .render())
    {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// When a path is not a page, list the pages under it as a namespace
/// (directory) if any exist; otherwise 404. Powers the OKF bundle index's
/// directory links and any relative `dir/` link (#603).
async fn namespace_or_not_found(
    state: &Arc<WebState>,
    workspace: &str,
    project: &str,
    path: &str,
) -> Response {
    let namespace = path.trim_end_matches('/');
    if namespace.is_empty() {
        return not_found_response();
    }
    let prefix = format!("{namespace}/");
    let all = match state.reader.list_pages(workspace, project).await {
        Ok(p) => p,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let pages: Vec<PageRow> = all
        .into_iter()
        .filter(|p| p.path.starts_with(&prefix))
        .map(|p| PageRow {
            href: page_href(workspace, project, &p.path),
            path: p.path,
            title: p.title,
            kind: p.kind,
            updated_relative: humanize(&p.updated_at),
        })
        .collect();
    if pages.is_empty() {
        return not_found_response();
    }
    match (NamespaceView {
        workspace: workspace.to_owned(),
        project: project.to_owned(),
        project_href: project_href(workspace, project),
        namespace: namespace.to_owned(),
        pages,
    }
    .render())
    {
        Ok(html) => Html(html).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

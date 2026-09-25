//! `GET /w/:workspace/:project/paginas` — page tree + recent activity. The
//! bare `/w/:workspace/:project` is the project overview
//! (`painel_overview::handler`); this screen is the "Páginas" tab.

use std::collections::BTreeMap;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;

use ai_memory_store::is_system_page;

use crate::state::WebState;
use crate::templates::{Folder, PageRow, ProjectView, humanize, page_href, project_href};

/// Handler for `GET /w/:workspace/:project/paginas`.
pub(crate) async fn handler(
    State(state): State<Arc<WebState>>,
    Path((workspace, project)): Path<(String, String)>,
) -> Result<Html<String>, StatusCode> {
    let pages = state
        .reader
        .list_pages(&workspace, &project)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Build sidebar folder trees (group by first path segment), split
    // into knowledge and machinery. A store accumulates far more
    // machinery pages (lint reports, session captures, monthly logs,
    // bundle indexes) than curated knowledge; listing them as peers
    // buried the concepts/decisions/rules a human actually opens this
    // UI for.
    let mut knowledge_map: BTreeMap<String, Vec<PageRow>> = BTreeMap::new();
    let mut system_map: BTreeMap<String, Vec<PageRow>> = BTreeMap::new();
    for p in &pages {
        let folder = p
            .path
            .split('/')
            .next()
            .and_then(|seg| {
                // Only treat it as a folder prefix if there's a slash in the path.
                if p.path.contains('/') {
                    Some(seg.to_owned())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "(root)".to_owned());
        let map = if is_system_page(&p.path) {
            &mut system_map
        } else {
            &mut knowledge_map
        };
        map.entry(folder).or_default().push(PageRow {
            path: p.path.clone(),
            href: page_href(&workspace, &project, &p.path),
            title: p.title.clone(),
            kind: p.kind.clone(),
            updated_relative: humanize(&p.updated_at),
        });
    }
    let folders: Vec<Folder> = knowledge_map
        .into_iter()
        .map(|(name, pages)| Folder { name, pages })
        .collect();
    let system: Vec<Folder> = system_map
        .into_iter()
        .map(|(name, pages)| Folder { name, pages })
        .collect();

    // Recent pages: knowledge only, sorted by updated_at desc, take 20.
    // Machinery updates constantly (logs append every consolidation,
    // lint reruns daily), so an unfiltered recency sort would show
    // nothing else.
    let mut sorted: Vec<_> = pages
        .iter()
        .filter(|p| !is_system_page(&p.path))
        .cloned()
        .collect();
    sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sorted.truncate(20);
    let recent: Vec<PageRow> = sorted
        .into_iter()
        .map(|p| PageRow {
            path: p.path.clone(),
            href: page_href(&workspace, &project, &p.path),
            title: p.title.clone(),
            kind: p.kind.clone(),
            updated_relative: humanize(&p.updated_at),
        })
        .collect();

    let base_href = project_href(&workspace, &project);
    let html = ProjectView {
        workspace,
        project,
        folders,
        system,
        recent,
        base_href,
        aba: "paginas",
    }
    .render()
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Html(html))
}

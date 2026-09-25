//! `GET /w/:workspace/:project` — the project overview ("Visão geral"), the
//! entry point of the project screen. The page tree moved to
//! `project::handler` at `/w/:workspace/:project/paginas`
//! (`docs/alfama/specs/2026-09-25-visao-geral-design.md` §2.2, §2.4).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use ai_memory_core::SlotVisibility;
use ai_memory_store::{
    BriefPageBody, OverviewPage, ProjectOverview, WeekChanges, brief, weekly_changes,
};
use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

use crate::routes::escopo_html;
use crate::state::WebState;
use crate::templates::{
    KindCount, OverviewPageRow, OverviewView, PAGE_KIND_ORDER, PeriodRow, TopSessionRow,
    WeekChangesRow, kind_label_pt, page_href, plural_pt, project_href,
};

/// Weeks shown in "Últimas grandes mudanças" (§2.2 item 2).
const MAX_WEEKS: usize = 4;

/// Cap for "Decisões recentes" and "Gotchas recentes" (§2.2 items 3 and 5).
const RECENT_LIMIT: usize = 10;

/// UTC calendar date (`YYYY-MM-DD`) of a microsecond timestamp. Falls back
/// to the empty string on an out-of-range value, which cannot happen for a
/// timestamp the store itself produced.
fn utc_date(us: i64) -> String {
    jiff::Timestamp::from_microsecond(us)
        .map(|ts| ts.strftime("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Folder (first path segment) on the Páginas tab that a page kind's tree
/// lives under, for the "Em números"/"Sem data de origem" anchors —
/// mirrors the path prefixes `ai_memory_store::painel::is_system_page` and
/// `page_kind_expr` already encode, without a parallel classification rule:
/// this only ever runs on a kind the store itself already computed for the
/// page, and is used strictly to link back to it.
fn folder_for_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "decision" => Some("decisions"),
        "concept" => Some("concepts"),
        "gotcha" => Some("gotchas"),
        "rule" => Some("_rules"),
        "procedure" => Some("procedures"),
        _ => None,
    }
}

/// Link to the folder anchor for `kind` on the Páginas tab, or the plain
/// Páginas link when `kind` has no single matching folder.
fn folder_href(base_href: &str, kind: &str) -> String {
    match folder_for_kind(kind) {
        Some(folder) => format!("{base_href}/paginas#pasta-{folder}"),
        None => format!("{base_href}/paginas"),
    }
}

/// Turn one [`OverviewPage`] into a link row, with an optional summary/date
/// (weekly-change and core-page rows show neither).
fn page_row(
    workspace: &str,
    project: &str,
    page: &OverviewPage,
    with_summary_and_date: bool,
) -> OverviewPageRow {
    OverviewPageRow {
        href: page_href(workspace, project, &page.path),
        title: page.title.clone(),
        kind: page.kind.clone(),
        summary: if with_summary_and_date {
            page.summary.clone()
        } else {
            None
        },
        date: if with_summary_and_date {
            page.origin_us.map(utc_date)
        } else {
            None
        },
    }
}

/// "Em números": one count per [`PAGE_KIND_ORDER`] kind, plus the session
/// count — always the full fixed order, zero counts included, so nothing is
/// silently dropped from the at-a-glance summary.
fn kind_counts(overview: &ProjectOverview, base_href: &str) -> Vec<KindCount> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for page in &overview.pages {
        *counts.entry(page.kind.as_str()).or_insert(0) += 1;
    }
    let mut out: Vec<KindCount> = PAGE_KIND_ORDER
        .iter()
        .map(|&kind| KindCount {
            label: kind_label_pt(kind, counts.get(kind).copied().unwrap_or(0)),
            href: folder_href(base_href, kind),
        })
        .collect();
    out.push(KindCount {
        label: plural_pt(overview.sessions.len(), "sessão", "sessões"),
        href: format!("{base_href}/paginas#pasta-sessions"),
    });
    out
}

/// "Sem data de origem": one count per kind among pages with no origin date
/// at all — [`PAGE_KIND_ORDER`] kinds first (only when non-zero), then any
/// other kind sorted alphabetically. Empty when every page has an origin.
fn missing_origin(overview: &ProjectOverview, base_href: &str) -> Vec<KindCount> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for page in &overview.pages {
        if page.origin_us.is_none() {
            *counts.entry(page.kind.clone()).or_insert(0) += 1;
        }
    }
    let mut out = Vec::new();
    for kind in PAGE_KIND_ORDER {
        if let Some(count) = counts.remove(kind) {
            out.push(KindCount {
                label: kind_label_pt(kind, count),
                href: folder_href(base_href, kind),
            });
        }
    }
    for (kind, count) in counts {
        out.push(KindCount {
            label: kind_label_pt(&kind, count),
            href: folder_href(base_href, &kind),
        });
    }
    out
}

/// The 10 most recent pages of `kind` by origin date (pages with no origin
/// date never qualify — they surface in "Sem data de origem" instead), ties
/// broken by path for determinism.
fn recent_by_kind(
    overview: &ProjectOverview,
    kind: &str,
    workspace: &str,
    project: &str,
) -> Vec<OverviewPageRow> {
    let mut pages: Vec<&OverviewPage> = overview
        .pages
        .iter()
        .filter(|p| p.kind == kind && p.origin_us.is_some())
        .collect();
    pages.sort_by(|a, b| {
        b.origin_us
            .cmp(&a.origin_us)
            .then_with(|| a.path.cmp(&b.path))
    });
    pages.truncate(RECENT_LIMIT);
    pages
        .into_iter()
        .map(|p| page_row(workspace, project, p, true))
        .collect()
}

/// "Últimas grandes mudanças": `weekly_changes` rendered into view rows.
fn week_rows(weeks: Vec<WeekChanges>, workspace: &str, project: &str) -> Vec<WeekChangesRow> {
    weeks
        .into_iter()
        .map(|w| WeekChangesRow {
            start_date: w.start_date,
            sessions_label: plural_pt(w.sessions as usize, "sessão", "sessões"),
            new_decisions: w
                .new_decisions
                .iter()
                .map(|p| page_row(workspace, project, p, false))
                .collect(),
            new_or_updated_concepts: w
                .new_or_updated_concepts
                .iter()
                .map(|p| page_row(workspace, project, p, false))
                .collect(),
            top_session: w.top_session.map(|(_, _, agent, produced)| TopSessionRow {
                agent,
                pages_label: plural_pt(produced as usize, "página", "páginas"),
            }),
        })
        .collect()
}

/// "Conceitos centrais": the briefing's core pages, in the briefing's own
/// order — the same call the Briefing screen makes
/// (`ReaderPool::session_brief_pages_with_slot_visibility`), never a
/// parallel query. A core page's kind is looked up in `overview` (the same
/// `page_kind_expr` result the rest of this screen uses); a core page the
/// overview does not carry at all (a pinned system page, or a `_slots/`
/// page — both excluded from `ProjectOverview::pages` by
/// `is_system_page`) falls back to the generic label "outro" rather than
/// guessing a kind from its path.
fn core_pages_section(
    core: &[BriefPageBody],
    overview: &ProjectOverview,
    workspace: &str,
    project: &str,
) -> (&'static str, bool, Vec<OverviewPageRow>) {
    let kind_by_path: HashMap<&str, &str> = overview
        .pages
        .iter()
        .map(|p| (p.path.as_str(), p.kind.as_str()))
        .collect();
    let rows: Vec<OverviewPageRow> = core
        .iter()
        .map(|p| OverviewPageRow {
            href: page_href(workspace, project, &p.path),
            title: p.title.clone(),
            kind: kind_by_path
                .get(p.path.as_str())
                .copied()
                .unwrap_or("outro")
                .to_owned(),
            summary: None,
            date: None,
        })
        .collect();
    let all_concepts = rows.iter().all(|p| p.kind == "concept");
    let title = if all_concepts {
        "Conceitos centrais"
    } else {
        "Páginas centrais (nem todas são conceitos)"
    };
    (title, !all_concepts, rows)
}

/// Handler for `GET /w/:workspace/:project`.
pub(crate) async fn handler(
    State(state): State<Arc<WebState>>,
    Path((workspace, project)): Path<(String, String)>,
) -> Response {
    let (ws, proj) = match escopo_html(&state, &workspace, &project).await {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    let overview = match state.reader.project_overview(ws, proj).await {
        Ok(o) => o,
        Err(err) => {
            tracing::error!(error = %err, "building project overview");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let (core, _recent) = match state
        .reader
        .session_brief_pages_with_slot_visibility(
            ws,
            proj,
            brief::BRIEF_CORE_PAGES_LIMIT,
            brief::BRIEF_RECENT_PAGES_LIMIT,
            SlotVisibility::All,
        )
        .await
    {
        Ok(pages) => pages,
        Err(err) => {
            tracing::error!(error = %err, "loading project overview core pages");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let base_href = project_href(&workspace, &project);
    let period = overview
        .first_session_us
        .zip(overview.last_session_us)
        .map(|(first, last)| PeriodRow {
            first: utc_date(first),
            last: utc_date(last),
        });
    let (core_title, core_show_kind, core_pages) =
        core_pages_section(&core, &overview, &workspace, &project);

    let view = OverviewView {
        kind_counts: kind_counts(&overview, &base_href),
        period,
        weeks: week_rows(weekly_changes(&overview, MAX_WEEKS), &workspace, &project),
        recent_decisions: recent_by_kind(&overview, "decision", &workspace, &project),
        core_title,
        core_show_kind,
        core_pages,
        recent_gotchas: recent_by_kind(&overview, "gotcha", &workspace, &project),
        missing_origin: missing_origin(&overview, &base_href),
        workspace,
        project,
        base_href,
        aba: "visao",
    };
    match view.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(error = %err, "rendering project overview");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

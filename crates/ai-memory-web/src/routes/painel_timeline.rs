//! `GET /w/:workspace/:project/linha-do-tempo` — sessions per UTC day, and
//! the current pages each session produced.

use std::collections::BTreeMap;
use std::sync::Arc;

use ai_memory_store::{ReaderPool, TimelineSession};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::routes::escopo_html;
use crate::state::WebState;
use crate::templates::{
    ProducedPageRow, TimelineDay, TimelineSessionRow, TimelineView, page_href, project_href,
};

/// Microseconds in a day, used to turn `?dias=` into `since_us`.
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// The only accepted `?dias=` values; anything else falls back to
/// [`DEFAULT_DIAS`].
const ALLOWED_DIAS: [i64; 3] = [7, 30, 90];

/// Fallback window when `?dias=` is absent, unparsable, or not one of
/// [`ALLOWED_DIAS`].
const DEFAULT_DIAS: i64 = 30;

/// Query string: `?dias=` — kept as a raw string so an unparsable value
/// falls back exactly like an out-of-range one, instead of axum rejecting
/// the whole request with a 400 for a screen that only ever reads.
#[derive(Deserialize)]
pub(crate) struct Params {
    dias: Option<String>,
}

/// Clamp the requested window to `{7, 30, 90}`, defaulting to 30 for
/// anything else — including a value that fails to parse as an integer.
fn clamp_dias(raw: Option<&str>) -> i64 {
    raw.and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|dias| ALLOWED_DIAS.contains(dias))
        .unwrap_or(DEFAULT_DIAS)
}

/// `"Xh YYmin"`/`"Xmin"`/`"Xs"` for an ended session, `"em aberto"` for one
/// still running.
fn duration_label(started_us: i64, ended_us: Option<i64>) -> String {
    let Some(ended_us) = ended_us else {
        return "em aberto".to_owned();
    };
    let secs = (ended_us - started_us).max(0) / 1_000_000;
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}min");
    }
    let hours = mins / 60;
    format!("{hours}h{:02}min", mins % 60)
}

/// The observation count, or `"—"` for an open session (whose persisted
/// count is a not-yet-meaningful placeholder — see
/// [`TimelineSession::observations`]).
fn observations_label(observations: Option<i64>) -> String {
    observations.map_or_else(|| "—".to_owned(), |n| n.to_string())
}

/// UTC calendar day (`YYYY-MM-DD`) a session started on. Falls back to the
/// empty string on an out-of-range timestamp, which cannot happen for a
/// value the store itself produced from `sessions.started_at`.
fn day_key(started_us: i64) -> String {
    jiff::Timestamp::from_microsecond(started_us)
        .map(|ts| ts.strftime("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Group sessions by UTC day (most recent day first) and compute each day's
/// bar width relative to the day with the most sessions in the window.
fn group_by_day(
    sessions: Vec<TimelineSession>,
    workspace: &str,
    project: &str,
) -> Vec<TimelineDay> {
    let mut by_day: BTreeMap<String, Vec<TimelineSession>> = BTreeMap::new();
    for session in sessions {
        by_day
            .entry(day_key(session.started_us))
            .or_default()
            .push(session);
    }
    let max_count = by_day.values().map(Vec::len).max().unwrap_or(0).max(1);
    by_day
        .into_iter()
        .rev()
        .map(|(date, sessions)| {
            let count = sessions.len();
            let rows = sessions
                .into_iter()
                .map(|s| TimelineSessionRow {
                    agent: s.agent,
                    duration_label: duration_label(s.started_us, s.ended_us),
                    observations_label: observations_label(s.observations),
                    produced: s
                        .produced
                        .into_iter()
                        .map(|p| ProducedPageRow {
                            href: page_href(workspace, project, &p.path),
                            path: p.path,
                            title: p.title,
                            kind: p.kind,
                        })
                        .collect(),
                })
                .collect();
            TimelineDay {
                date,
                count,
                pct: (count * 100 / max_count).min(100),
                sessions: rows,
            }
        })
        .collect()
}

/// Handler for `GET /w/:workspace/:project/linha-do-tempo`.
pub(crate) async fn handler(
    State(state): State<Arc<WebState>>,
    Path((workspace, project)): Path<(String, String)>,
    Query(params): Query<Params>,
) -> Response {
    let (ws, proj) = match escopo_html(&state, &workspace, &project).await {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    let dias = clamp_dias(params.dias.as_deref());
    let since_us = jiff::Timestamp::now().as_microsecond() - dias * MICROS_PER_DAY;
    let sessions = match reader_timeline(&state.reader, ws, proj, since_us).await {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(error = %err, "building timeline");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let view = TimelineView {
        base_href: project_href(&workspace, &project),
        aba: "linha",
        dias,
        days: group_by_day(sessions, &workspace, &project),
        workspace,
        project,
    };
    match view.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(error = %err, "rendering timeline");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Thin wrapper kept as its own function so tests can call the exact query
/// the handler uses without going through HTTP, mirroring the briefing
/// screen's `montar`.
async fn reader_timeline(
    reader: &ReaderPool,
    ws: ai_memory_core::WorkspaceId,
    proj: ai_memory_core::ProjectId,
    since_us: i64,
) -> anyhow::Result<Vec<TimelineSession>> {
    Ok(reader.timeline(ws, proj, since_us).await?)
}

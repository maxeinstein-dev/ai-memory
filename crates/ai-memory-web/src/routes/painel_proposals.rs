//! `GET /w/:workspace/:project/propostas` — the read-only queue of pending
//! (or otherwise filtered) auto-improvement proposals for one project.
//!
//! This screen never writes. It shows the exact `ai-memory pending-writes
//! approve/reject <ID>` commands as selectable text so an operator decides
//! and acts from the CLI — the web stays a read surface, same as every
//! other panel screen (spec §3.3, §6).

use std::str::FromStr;
use std::sync::Arc;

use ai_memory_core::{ProjectId, WorkspaceId};
use ai_memory_store::{
    AutoImproveProposalDetail, AutoImproveProposalOperation, AutoImproveProposalStatus, ReaderPool,
};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::routes::escopo_html;
use crate::state::WebState;
use crate::templates::{EvidenceRow, ProposalCard, ProposalsView, page_href, project_href};

/// Query string: `?status=` — kept as a raw string so an unparsable or
/// unknown value falls back to `pending` instead of axum rejecting the
/// whole request, matching the timeline screen's `?dias=` handling.
#[derive(Deserialize)]
pub(crate) struct Params {
    status: Option<String>,
}

/// Parse `?status=` via [`AutoImproveProposalStatus::from_str`]; anything
/// absent, empty, or unrecognised falls back to `Pending` (spec §3.3).
fn clamp_status(raw: Option<&str>) -> AutoImproveProposalStatus {
    raw.and_then(|v| AutoImproveProposalStatus::from_str(v.trim()).ok())
        .unwrap_or(AutoImproveProposalStatus::Pending)
}

/// SHA-256 of a page body, using the exact same hashing the store uses for
/// `pages.body_sha256` (`upsert_page_in_tx` in `ops.rs`: plain SHA-256 over
/// the raw body bytes) — so a hash computed here from the live body compares
/// correctly against a proposal's `target_body_sha256_at_stage`.
fn body_sha256(body: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    hasher.finalize().into()
}

/// Evidence entries are `[{ "page": "...", "quote": "..." }, ...]`
/// (`AutoImproveEvidence` in `ai-memory-consolidate`); this screen only
/// reads the JSON, so it parses it defensively rather than depending on
/// that crate.
fn evidence_rows(
    workspace: &str,
    project: &str,
    evidence_json: &serde_json::Value,
) -> Vec<EvidenceRow> {
    let Some(items) = evidence_json.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            let page = item
                .get("page")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let quote = item
                .get("quote")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            // Only a `sessions/<id>.md` citation gets a link, and it is
            // always built through `page_href` (percent-encoded segments)
            // rather than interpolated straight into an `href` attribute.
            let href = (page.starts_with("sessions/") && page.ends_with(".md"))
                .then(|| page_href(workspace, project, &page));
            EvidenceRow { page, quote, href }
        })
        .collect()
}

/// Build one card from a proposal's full detail, resolving the "before" body
/// and conflict flag for `update` proposals along the way.
async fn build_card(
    reader: &ReaderPool,
    ws: WorkspaceId,
    proj: ProjectId,
    workspace: &str,
    project: &str,
    detail: AutoImproveProposalDetail,
) -> anyhow::Result<ProposalCard> {
    let (before, conflict) = match detail.summary.operation {
        AutoImproveProposalOperation::Create => (None, false),
        AutoImproveProposalOperation::Update => {
            let current = reader
                .page_body_by_ids(ws, proj, detail.summary.target_path.as_str())
                .await?;
            match current {
                Some(page) => {
                    let conflict = match detail.target_body_sha256_at_stage {
                        Some(staged) => body_sha256(&page.body) != staged,
                        None => true,
                    };
                    (Some(page.body), conflict)
                }
                // Target vanished since staging — as much a conflict as a
                // changed body; there is no "before" to show.
                None => (None, true),
            }
        }
    };
    Ok(ProposalCard {
        id: detail.summary.id.to_string(),
        title: detail.summary.title,
        kind: detail.summary.kind,
        operation: detail.summary.operation.as_str(),
        target_path: detail.summary.target_path.as_str().to_owned(),
        confidence_pct: (detail.summary.confidence * 100.0).round() as i64,
        conflict,
        rationale: detail.rationale,
        evidence: evidence_rows(workspace, project, &detail.evidence_json),
        before,
        after: detail.body_markdown,
        approve_cmd: format!("ai-memory pending-writes approve {}", detail.summary.id),
        reject_cmd: format!("ai-memory pending-writes reject {}", detail.summary.id),
    })
}

/// Handler for `GET /w/:workspace/:project/propostas`.
pub(crate) async fn handler(
    State(state): State<Arc<WebState>>,
    Path((workspace, project)): Path<(String, String)>,
    Query(params): Query<Params>,
) -> Response {
    let (ws, proj) = match escopo_html(&state, &workspace, &project).await {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    let status = clamp_status(params.status.as_deref());
    let summaries = match state
        .reader
        .list_auto_improve_proposals(ws, proj, Some(status), 100)
        .await
    {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(error = %err, "listing auto-improve proposals");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let mut proposals = Vec::with_capacity(summaries.len());
    for summary in summaries {
        let detail = match state
            .reader
            .auto_improve_proposal_detail(ws, proj, summary.id)
            .await
        {
            Ok(Some(detail)) => detail,
            // Listed a moment ago, gone now (raced a decision) — skip
            // rather than 500 a read-only listing over a race.
            Ok(None) => continue,
            Err(err) => {
                tracing::error!(error = %err, "reading auto-improve proposal detail");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
        match build_card(&state.reader, ws, proj, &workspace, &project, detail).await {
            Ok(card) => proposals.push(card),
            Err(err) => {
                tracing::error!(error = %err, "building proposal card");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }
    let view = ProposalsView {
        base_href: project_href(&workspace, &project),
        aba: "propostas",
        status: status.as_str(),
        proposals,
        workspace,
        project,
    };
    match view.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(error = %err, "rendering proposals");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

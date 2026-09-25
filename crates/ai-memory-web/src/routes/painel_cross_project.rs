//! `GET /entre-projetos` — the one cross-project screen (spec §3.4): rule
//! pages that look the same across two or more projects, currently open
//! handoffs, and pending cross-project messages.
//!
//! Unlike every other panel screen, this one is top-level (not scoped to
//! `/w/:workspace/:project`) — it aggregates over every project the home page
//! lists (`ReaderPool::list_projects_with_stats`, reused as-is). Rule grouping
//! reuses `ReaderPool::rules_across_projects` + `group_rules` (Tarefa 13) and
//! never crosses workspaces. Handoffs reuse the EXACT owner filter and body
//! redaction the JSON API applies (`owner_filter_for`, `serves_handoff_body`
//! in `routes::api`) — invariant 16: `OwnerFilter` applies to handoffs only,
//! never to pages or messages.

use std::sync::Arc;

use ai_memory_core::{ActorContext, AuthLevel, HandoffState, MessageBox, OwnerFilter};
use ai_memory_store::{ReaderPool, RuleGroup, group_rules, lookup_existing_scope};
use askama::Template;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

use crate::routes::api::{owner_filter_for, serves_handoff_body};
use crate::state::WebState;
use crate::templates::{
    CrossProjectView, OpenHandoffRow, PendingMessageRow, RuleGroupRow, RuleMemberRow, humanize_pt,
    page_href, project_href,
};

/// Same threshold the spec fixes for rule grouping (§3.4).
const RULE_SIMILARITY_THRESHOLD: f32 = 0.85;
/// Per-project cap on each listing — generous for a cross-project overview
/// screen without risking an unbounded page for a very active install.
const PER_PROJECT_LIMIT: usize = 50;

/// Build the "Regras candidatas a globais" section. Grouping runs once per
/// workspace and never mixes workspaces (`rules_across_projects` is already
/// scoped to one `WorkspaceId`; spec §3.4 fixes the grouping to be
/// per-workspace even though this screen aggregates every workspace).
async fn build_rule_groups(
    reader: &ReaderPool,
    workspace_names: &[String],
) -> ai_memory_store::StoreResult<Vec<RuleGroupRow>> {
    let mut rows = Vec::new();
    for ws_name in workspace_names {
        let Some(ws_id) = reader.find_workspace(ws_name.clone()).await? else {
            continue;
        };
        let candidates = reader.rules_across_projects(ws_id).await?;
        for group in group_rules(candidates, RULE_SIMILARITY_THRESHOLD) {
            rows.push(build_rule_group_row(ws_name, group));
        }
    }
    Ok(rows)
}

fn build_rule_group_row(workspace: &str, group: RuleGroup) -> RuleGroupRow {
    let members = group
        .rules
        .into_iter()
        .map(|r| RuleMemberRow {
            href: page_href(workspace, &r.project, &r.path),
            project: r.project,
            title: r.title,
            path: r.path,
        })
        .collect();
    RuleGroupRow {
        workspace: workspace.to_owned(),
        members,
    }
}

/// Append one project's open handoffs and pending inbox messages to the
/// running totals.
///
/// # Handoffs
/// Same `owner_filter`/`with_body` the JSON API's `handoffs_handler` computes
/// once per request (`owner_filter_for`, `serves_handoff_body`): an actor sees
/// their own open handoffs plus the shared ones, never another operator's,
/// and the summary is shown only when the API would let this caller read it.
///
/// # Messages
/// `ReaderPool::list_messages` scopes strictly by mailbox coordinate
/// (`to_workspace_id`/`to_project_id` for the inbox side) — there is no
/// owner/actor parameter to pass. `AgentMessage::origin.from_owner_user` is
/// documented as "Attribution/provenance only — never a read filter"
/// (`crates/ai-memory-core/src/message.rs`), and the module doc there says
/// mail crossing is scoped only by "a project only ever reads mail addressed
/// TO it ... or sent FROM it" — never further by who sent or will read it
/// within that project. So messages are project-shared by design, same as
/// every other page (invariant 16), and this screen shows every pending
/// message addressed to a listed project regardless of the requesting actor.
async fn collect_handoffs_and_messages(
    reader: &ReaderPool,
    workspace: &str,
    project: &str,
    owner_filter: &OwnerFilter,
    with_body: bool,
    handoffs: &mut Vec<OpenHandoffRow>,
    messages: &mut Vec<PendingMessageRow>,
) -> anyhow::Result<()> {
    let scope = match lookup_existing_scope(reader, workspace, project).await {
        Ok(scope) => scope,
        // Raced with a delete between the listing and here — skip rather
        // than fail the whole cross-project screen over one vanished project.
        Err(err) if err.is_not_found() => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let (ws_id, proj_id) = scope.as_tuple();

    let open = reader
        .list_handoffs(
            ws_id,
            proj_id,
            Some(HandoffState::Open),
            owner_filter.clone(),
            PER_PROJECT_LIMIT,
        )
        .await?;
    for h in open {
        handoffs.push(OpenHandoffRow {
            workspace: workspace.to_owned(),
            project: project.to_owned(),
            project_href: project_href(workspace, project),
            agent: h.origin.from_agent.as_str().to_owned(),
            summary: with_body.then_some(h.content.summary),
            age: humanize_pt(&h.lifecycle.created_at.to_string()),
        });
    }

    let pending = reader
        .list_messages(ws_id, proj_id, MessageBox::Inbox, PER_PROJECT_LIMIT)
        .await?;
    for m in pending {
        let from_project = reader
            .project_name_by_id(m.origin.from_workspace_id, m.origin.from_project_id)
            .await?
            .unwrap_or_default();
        let from_workspace = reader
            .workspace_name_by_id(m.origin.from_workspace_id)
            .await?
            .unwrap_or_default();
        let summary = m.subject.filter(|s| !s.trim().is_empty()).unwrap_or(m.body);
        messages.push(PendingMessageRow {
            from_workspace,
            from_project,
            to_workspace: workspace.to_owned(),
            to_project: project.to_owned(),
            summary,
            age: humanize_pt(&m.created_at.to_string()),
        });
    }
    Ok(())
}

/// Handler for `GET /entre-projetos`.
pub(crate) async fn handler(
    State(state): State<Arc<WebState>>,
    actor: Option<Extension<ActorContext>>,
    auth: Option<Extension<AuthLevel>>,
) -> Response {
    // Same project listing the home page uses (index::handler).
    let summaries = match state.reader.list_projects_with_stats().await {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(error = %err, "listing projects for the cross-project screen");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let mut workspace_names: Vec<String> =
        summaries.iter().map(|s| s.workspace_name.clone()).collect();
    workspace_names.sort();
    workspace_names.dedup();

    let rule_groups = match build_rule_groups(&state.reader, &workspace_names).await {
        Ok(g) => g,
        Err(err) => {
            tracing::error!(error = %err, "grouping cross-project rules");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let owner_filter = owner_filter_for(actor);
    let with_body = serves_handoff_body(&owner_filter, auth);

    let mut open_handoffs = Vec::new();
    let mut pending_messages = Vec::new();
    for s in &summaries {
        if let Err(err) = collect_handoffs_and_messages(
            &state.reader,
            &s.workspace_name,
            &s.project_name,
            &owner_filter,
            with_body,
            &mut open_handoffs,
            &mut pending_messages,
        )
        .await
        {
            tracing::error!(error = %err, "collecting handoffs/messages for the cross-project screen");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    let view = CrossProjectView {
        rule_groups,
        open_handoffs,
        pending_messages,
    };
    match view.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(error = %err, "rendering cross-project screen");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

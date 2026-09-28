//! Cross-agent handoff type.
//!
//! A handoff is a typed snapshot of "where we are" — created when one
//! agent CLI ends a session, accepted when the next one starts in the
//! same project. Stored explicitly (vs. inferring from the
//! observations log) because cross-agent continuity is the project's
//! headline feature and deserves a first-class schema.

use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{
    OwnerFilter,
    ids::{AgentKind, HandoffId, ProjectId, SessionId, WorkspaceId},
};

/// State machine of a single handoff row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffState {
    /// Created, not yet picked up by the next agent.
    Open,
    /// Another agent has called `memory_handoff_accept` on it.
    Accepted,
    /// Aged out (decay sweep).
    Expired,
}

impl HandoffState {
    /// Canonical wire string.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Accepted => "accepted",
            Self::Expired => "expired",
        }
    }
}

impl std::str::FromStr for HandoffState {
    type Err = crate::MemoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" => Ok(Self::Open),
            "accepted" => Ok(Self::Accepted),
            "expired" => Ok(Self::Expired),
            other => Err(crate::MemoryError::MalformedRecord(format!(
                "unknown handoff state: {other}"
            ))),
        }
    }
}

/// Input for inserting a new handoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewHandoff {
    /// Owning workspace.
    pub workspace_id: WorkspaceId,
    /// Owning project.
    pub project_id: ProjectId,
    /// Session this handoff captures (None for manual handoffs).
    pub from_session_id: Option<SessionId>,
    /// Agent CLI that produced this handoff.
    pub from_agent: AgentKind,
    /// Optional explicit target hint (`claude-code`, `codex`, …).
    pub to_agent: Option<AgentKind>,
    /// Working directory at handoff time. Used to match the next
    /// session's `memory_handoff_accept` call.
    pub cwd: Option<PathBuf>,
    /// One-paragraph summary of where we left off.
    pub summary: String,
    /// Open questions for the next agent.
    pub open_questions: Vec<String>,
    /// Suggested next steps.
    pub next_steps: Vec<String>,
    /// Files touched in the session.
    pub files_touched: Vec<String>,
    /// Operator this handoff belongs to, as an
    /// [`crate::IdentityKey::storage_key`] string. `None` publishes it to the
    /// whole project (the pre-ownership behaviour, and what a caller with no
    /// actor produces).
    #[serde(default)]
    pub owner_user: Option<String>,
}

/// Scope, ownership, and receiver metadata for an atomic handoff claim.
#[derive(Debug, Clone)]
pub struct HandoffAcceptance {
    /// Handoff being claimed.
    pub handoff_id: HandoffId,
    /// Workspace the caller resolved before the claim.
    pub workspace_id: WorkspaceId,
    /// Project the caller resolved before the claim.
    pub project_id: ProjectId,
    /// Agent CLI accepting the handoff.
    pub accepting_agent: AgentKind,
    /// Session accepting the handoff, when known.
    pub accepting_session: Option<SessionId>,
    /// Operator accepting the handoff, in [`crate::IdentityKey::storage_key`]
    /// form.
    pub accepting_user: Option<String>,
    /// Ownership boundary the caller is authorized to claim through.
    pub owner_filter: OwnerFilter,
    /// Working directory of the receiving session, used to bound automatic
    /// handoff supersession.
    pub receiving_cwd: Option<String>,
}

/// Row identity and tenancy for a handoff.
#[derive(Debug, Clone, Serialize)]
pub struct HandoffScope {
    /// Stable identifier.
    pub id: HandoffId,
    /// Owning workspace.
    pub workspace_id: WorkspaceId,
    /// Owning project.
    pub project_id: ProjectId,
}

/// Where a handoff came from and who it's for.
#[derive(Debug, Clone, Serialize)]
pub struct HandoffOrigin {
    /// Session that produced this handoff, if any.
    pub from_session_id: Option<SessionId>,
    /// Agent CLI that produced this handoff.
    pub from_agent: AgentKind,
    /// Optional target hint.
    pub to_agent: Option<AgentKind>,
    /// Working directory at handoff time.
    pub cwd: Option<String>,
    /// Operator this handoff belongs to ([`crate::IdentityKey::storage_key`]
    /// form); `None` means shared with the project.
    pub owner_user: Option<String>,
}

/// The handoff payload itself.
#[derive(Debug, Clone, Serialize)]
pub struct HandoffContent {
    /// Summary.
    pub summary: String,
    /// Open questions.
    pub open_questions: Vec<String>,
    /// Next steps.
    pub next_steps: Vec<String>,
    /// Files touched.
    pub files_touched: Vec<String>,
}

/// State machine and acceptance record for a handoff.
#[derive(Debug, Clone, Serialize)]
pub struct HandoffLifecycle {
    /// State.
    pub state: HandoffState,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Agent CLI that accepted, if any.
    pub accepted_by: Option<AgentKind>,
    /// Acceptance timestamp.
    pub accepted_at: Option<Timestamp>,
    /// Session that accepted, if any.
    pub accepted_by_session: Option<SessionId>,
    /// Operator that accepted it. Unlike [`HandoffLifecycle::accepted_by`]
    /// (the agent CLI), this answers "which teammate took the baton".
    pub accepted_by_user: Option<String>,
}

/// Materialised view of a handoff row.
///
/// Grouped into sub-structs (identity, origin, content, lifecycle) instead
/// of 18 flat fields; each is `#[serde(flatten)]`ed so the MCP wire JSON
/// stays exactly as flat as before this split.
#[derive(Debug, Clone, Serialize)]
pub struct Handoff {
    /// Row identity and tenancy.
    #[serde(flatten)]
    pub scope: HandoffScope,
    /// Where it came from and who it's for.
    #[serde(flatten)]
    pub origin: HandoffOrigin,
    /// The payload.
    #[serde(flatten)]
    pub content: HandoffContent,
    /// State machine and acceptance record.
    #[serde(flatten)]
    pub lifecycle: HandoffLifecycle,
}

// ---------------------------------------------------------------------------
// Free-text sanitization shared by every handoff-creating surface.
// ---------------------------------------------------------------------------
//
// A handoff bypasses `Wiki::write_page` (it lives in its own table), so every
// caller that creates one — the MCP `memory_handoff_begin` tool and the web
// panel's `POST /handoff` route — must scrub and cap its free-text fields
// itself. This used to be inline logic private to `ai-memory-mcp::server`;
// it moved here (the one crate every handoff-creating surface already
// depends on) so there is exactly one implementation instead of one per
// caller. See `docs/alfama/plans/2026-09-28-handoff-pela-tela.md` Tarefa 1.

use crate::Sanitizer;

/// Per-field character cap for the handoff `summary`.
pub const HANDOFF_SUMMARY_MAX_CHARS: usize = 3_000;
/// Per-item character cap for a single `open_questions`/`next_steps` entry.
pub const HANDOFF_ITEM_MAX_CHARS: usize = 1_500;
/// Per-item character cap for a single `files_touched` entry.
pub const HANDOFF_FILE_MAX_CHARS: usize = 512;
/// Total character cap across all of `open_questions` or `next_steps`.
pub const HANDOFF_TEXT_LIST_MAX_CHARS: usize = 6_000;
/// Total character cap across all of `files_touched`.
pub const HANDOFF_FILE_LIST_MAX_CHARS: usize = 4_096;
/// Maximum number of items kept from any handoff list field, independent of
/// the character budget above (a flood of one-char items would otherwise
/// slip past the char cap).
pub const HANDOFF_LIST_MAX_ITEMS: usize = 20;

/// Cap one user-visible string with a visible truncation marker.
///
/// Shared by every surface that needs to bound a free-text field without
/// silently dropping the tail: the omission is stated inline, not just
/// implied by a shorter string. Moved here from
/// `ai_memory_consolidate::projection` (re-exported there for its existing
/// callers) because this crate — not `ai-memory-consolidate` — is the one
/// every handoff-creating surface, including the web panel, already depends
/// on.
#[must_use]
pub fn cap_text_with_marker(input: &str, max_chars: usize, label: &str) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_chars).collect();
    let omitted = input.chars().count().saturating_sub(max_chars);
    out.push_str(&format!("\n[{label} truncated; {omitted} chars omitted]"));
    out
}

fn push_handoff_omission_marker(
    out: &mut Vec<String>,
    used_chars: &mut usize,
    total_max_chars: usize,
    label: &str,
    omitted: usize,
) {
    if omitted == 0 {
        return;
    }
    let separator = usize::from(!out.is_empty());
    let available = total_max_chars.saturating_sub(used_chars.saturating_add(separator));
    if available == 0 {
        return;
    }
    let marker = format!("[{label} truncated; {omitted} additional item(s) omitted]");
    let marker: String = marker.chars().take(available).collect();
    *used_chars = used_chars
        .saturating_add(separator)
        .saturating_add(marker.chars().count());
    out.push(marker);
}

/// Cap a list of already-capped-per-item strings to a total character
/// budget and a maximum item count, appending a visible omission marker
/// when either limit trims the list.
#[must_use]
pub fn cap_handoff_list<I>(
    items: I,
    item_max_chars: usize,
    total_max_chars: usize,
    item_label: &str,
    list_label: &str,
) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let capped: Vec<String> = items
        .into_iter()
        .map(|item| cap_text_with_marker(&item, item_max_chars, item_label))
        .collect();
    let total_items = capped.len();
    let mut out = Vec::new();
    let mut used_chars = 0usize;

    for (idx, item) in capped.into_iter().enumerate() {
        if out.len() >= HANDOFF_LIST_MAX_ITEMS {
            push_handoff_omission_marker(
                &mut out,
                &mut used_chars,
                total_max_chars,
                list_label,
                total_items.saturating_sub(idx),
            );
            break;
        }
        let item_len = item.chars().count();
        let separator = usize::from(!out.is_empty());
        if !out.is_empty()
            && used_chars
                .saturating_add(separator)
                .saturating_add(item_len)
                > total_max_chars
        {
            push_handoff_omission_marker(
                &mut out,
                &mut used_chars,
                total_max_chars,
                list_label,
                total_items.saturating_sub(idx),
            );
            break;
        }
        used_chars = used_chars
            .saturating_add(separator)
            .saturating_add(item_len);
        out.push(item);
    }
    out
}

/// Scrub + cap every free-text field of a handoff, in the same order every
/// caller must apply them (scrub first, then cap): `summary`,
/// `open_questions`, `next_steps`, `files_touched`. Returns the four fields
/// in that order, ready to drop straight into [`NewHandoff`].
///
/// This is the single implementation `memory_handoff_begin` (MCP) and
/// `POST /handoff` (web panel) both call — see the module docs above for
/// why it lives here instead of being duplicated per caller.
#[must_use]
pub fn sanitize_handoff_text_fields(
    sanitizer: &Sanitizer,
    summary: &str,
    open_questions: &[String],
    next_steps: &[String],
    files_touched: &[String],
) -> (String, Vec<String>, Vec<String>, Vec<String>) {
    let summary = cap_text_with_marker(
        &sanitizer.scrub(summary),
        HANDOFF_SUMMARY_MAX_CHARS,
        "handoff summary",
    );
    let open_questions = cap_handoff_list(
        open_questions.iter().map(|q| sanitizer.scrub(q)),
        HANDOFF_ITEM_MAX_CHARS,
        HANDOFF_TEXT_LIST_MAX_CHARS,
        "handoff item",
        "handoff open_questions",
    );
    let next_steps = cap_handoff_list(
        next_steps.iter().map(|n| sanitizer.scrub(n)),
        HANDOFF_ITEM_MAX_CHARS,
        HANDOFF_TEXT_LIST_MAX_CHARS,
        "handoff item",
        "handoff next_steps",
    );
    let files_touched = cap_handoff_list(
        files_touched.iter().map(|f| sanitizer.scrub(f)),
        HANDOFF_FILE_MAX_CHARS,
        HANDOFF_FILE_LIST_MAX_CHARS,
        "handoff file",
        "handoff files_touched",
    );
    (summary, open_questions, next_steps, files_touched)
}

#[cfg(test)]
mod sanitize_tests {
    use super::*;

    fn sanitizer() -> Sanitizer {
        Sanitizer::new(&crate::sanitize::SanitizeConfig::default())
            .expect("default sanitizer config must compile")
    }

    #[test]
    fn cap_text_with_marker_leaves_short_text_untouched() {
        assert_eq!(cap_text_with_marker("hello", 10, "x"), "hello");
    }

    #[test]
    fn cap_text_with_marker_truncates_and_marks_long_text() {
        let out = cap_text_with_marker(&"a".repeat(20), 5, "field");
        assert!(out.starts_with(&"a".repeat(5)));
        assert!(out.contains("[field truncated; 15 chars omitted]"));
    }

    #[test]
    fn cap_handoff_list_caps_item_count() {
        let items = (0..HANDOFF_LIST_MAX_ITEMS + 5).map(|i| i.to_string());
        let out = cap_handoff_list(items, 100, 100_000, "item", "list");
        // MAX_ITEMS kept + one omission marker.
        assert_eq!(out.len(), HANDOFF_LIST_MAX_ITEMS + 1);
        assert!(out.last().unwrap().contains("5 additional item(s) omitted"));
    }

    #[test]
    fn sanitize_handoff_text_fields_caps_summary() {
        let s = sanitizer();
        // Overage large enough that the visible truncation marker's own bytes
        // cannot mask the cap: a `+20` overage nets a *longer* capped string
        // once the marker text is appended, which is not a truncation bug —
        // just the wrong assertion. Use an overage that dwarfs the marker.
        let long_summary = "s".repeat(HANDOFF_SUMMARY_MAX_CHARS + 5_000);
        let (summary, _, _, _) = sanitize_handoff_text_fields(&s, &long_summary, &[], &[], &[]);
        assert!(summary.len() < long_summary.len());
        assert!(summary.contains("handoff summary truncated"));
    }

    #[test]
    fn sanitize_handoff_text_fields_caps_items() {
        let s = sanitizer();
        let long_item = "n".repeat(HANDOFF_ITEM_MAX_CHARS + 5_000);
        let (_, _, next_steps, _) =
            sanitize_handoff_text_fields(&s, "summary", &[], std::slice::from_ref(&long_item), &[]);
        assert_eq!(next_steps.len(), 1);
        assert!(next_steps[0].len() < long_item.len());
        assert!(next_steps[0].contains("handoff item truncated"));
    }
}

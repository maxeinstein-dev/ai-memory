//! `askama` template definitions and per-route view-models.

use askama::Template;

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

/// Build a project URL with path segments percent-encoded.
///
/// The URL is **relative** (`w/{ws}/{proj}`, no leading slash) so it
/// resolves against the page's injected `<base href>` — which the server
/// sets to `{base_path}{web_slug}/`. That keeps every link correct whether
/// the browser is served at the host root (`/web/…`) or under a reverse-proxy
/// subpath (`/wiki/web/…`), without the templates knowing the prefix.
#[must_use]
pub(crate) fn project_href(workspace: &str, project: &str) -> String {
    format!(
        "w/{}/{}",
        encode_segment(workspace),
        encode_segment(project)
    )
}

/// Build a `/web` page URL with workspace/project/path percent-encoded.
#[must_use]
pub(crate) fn page_href(workspace: &str, project: &str, path: &str) -> String {
    format!(
        "{}/p/{}",
        project_href(workspace, project),
        encode_path(path)
    )
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(&mut out, "%{byte:02X}");
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Humanise helper
// ---------------------------------------------------------------------------

/// Format an ISO-8601 timestamp string as a relative human-readable string
/// (e.g. "3 hours ago", "2 days ago"). Falls back to the raw string on any
/// parse error.
#[must_use]
pub(crate) fn humanize(iso: &str) -> String {
    let Ok(then) = iso.parse::<jiff::Timestamp>() else {
        return iso.to_owned();
    };
    let now = jiff::Timestamp::now();
    // Compute elapsed seconds using microsecond arithmetic to avoid Span API.
    let diff_us = now.as_microsecond() - then.as_microsecond();
    let secs = diff_us.abs() / 1_000_000;
    if secs < 60 {
        return "just now".to_owned();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins} minute{} ago", if mins == 1 { "" } else { "s" });
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" });
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{days} day{} ago", if days == 1 { "" } else { "s" });
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months} month{} ago", if months == 1 { "" } else { "s" });
    }
    let years = months / 12;
    format!("{years} year{} ago", if years == 1 { "" } else { "s" })
}

// ---------------------------------------------------------------------------
// projects.html
// ---------------------------------------------------------------------------

/// One card on the project-list page.
pub(crate) struct ProjectCard {
    /// Workspace name.
    pub workspace: String,
    /// Project name.
    pub project: String,
    /// Number of latest pages.
    pub page_count: u64,
    /// Humanised timestamp (e.g. "3 hours ago"), or empty string.
    pub last_updated_relative: String,
    /// Link target (`w/{ws}/{proj}`, relative to `<base href>`).
    pub href: String,
}

/// The one-time 2.0 migration explainer dialog (docs/okf.md): shown
/// whenever a migration receipt exists, dismissed per browser via a
/// "do not show me again" checkbox persisted in localStorage keyed by
/// the migration timestamp (a future migration re-shows it).
pub(crate) struct OkfDialog {
    /// Absolute archive path from the receipt.
    pub archive_path: String,
    /// Human-readable archive size.
    pub size_human: String,
    /// Migration timestamp — also the localStorage dismissal key.
    pub created_at: String,
    /// Whether the archive file still exists (adapts the recovery text).
    pub archive_present: bool,
}

/// View-model for `GET /`.
#[derive(Template)]
#[template(path = "projects.html")]
pub(crate) struct ProjectsView {
    /// All project cards, sorted by most recently active first.
    pub projects: Vec<ProjectCard>,
    /// Present whenever a migration receipt exists (dialog dismissal is
    /// client-side, per browser).
    pub okf_dialog: Option<OkfDialog>,
}

// ---------------------------------------------------------------------------
// project.html
// ---------------------------------------------------------------------------

/// One entry in the sidebar or recent-list.
pub(crate) struct PageRow {
    /// Relative wiki path.
    pub path: String,
    /// Link target for this page.
    pub href: String,
    /// Page title.
    pub title: String,
    /// Semantic kind badge text.
    pub kind: String,
    /// Humanised updated timestamp.
    pub updated_relative: String,
}

/// A folder in the sidebar tree (groups pages by first path segment).
pub(crate) struct Folder {
    /// Folder name (first path segment, without trailing slash).
    pub name: String,
    /// Pages inside this folder.
    pub pages: Vec<PageRow>,
}

/// View-model for `GET /w/:workspace/:project`.
#[derive(Template)]
#[template(path = "project.html")]
pub(crate) struct ProjectView {
    /// Workspace name.
    pub workspace: String,
    /// Project name.
    pub project: String,
    /// Sidebar folder tree — knowledge pages only.
    pub folders: Vec<Folder>,
    /// Machinery pages (lint reports, sessions, logs, indexes),
    /// rendered collapsed below the knowledge tree.
    pub system: Vec<Folder>,
    /// N most-recent knowledge pages for the right column.
    pub recent: Vec<PageRow>,
    /// Link target for this project (`w/{ws}/{proj}`), used by `_abas.html`
    /// to build the tab hrefs relative to the injected `<base href>`.
    pub base_href: String,
    /// Which panel-tab is active; `_abas.html` bolds the matching link.
    pub aba: &'static str,
}

// ---------------------------------------------------------------------------
// painel_briefing.html
// ---------------------------------------------------------------------------

/// One core page the session brief considered.
pub(crate) struct ItemDoBriefing {
    /// Relative wiki path.
    pub path: String,
    /// Page title.
    pub title: String,
    /// Trimmed body length in bytes — the same unit the renderer budgets
    /// against (`buf.len()` in `brief.rs`), not chars.
    pub bytes: usize,
    /// Whether this page's own body header appears in the rendered brief:
    /// matches the renderer's exact `` (`path`) `` body-header marker, not
    /// an approximation by title.
    pub entrou: bool,
}

/// View-model for `GET /w/:workspace/:project/briefing`.
#[derive(Template)]
#[template(path = "painel_briefing.html")]
pub(crate) struct BriefingView {
    /// Workspace name.
    pub workspace: String,
    /// Project name.
    pub project: String,
    /// Link target for this project, used by `_abas.html`.
    pub base_href: String,
    /// Active panel-tab for `_abas.html`.
    pub aba: &'static str,
    /// Bytes the brief uses (`markdown.len()` — the renderer budgets in
    /// bytes, not chars; accents and emoji cost more than one).
    pub usados: usize,
    /// Effective (clamped) byte budget.
    pub orcamento: usize,
    /// `usados` as a percentage of `orcamento`, capped at 100 — drives the
    /// inline `width` of the usage bar.
    pub pct: usize,
    /// `ai_memory_store::brief::BRIEF_BUDGET_DEFAULT` — the budget this
    /// preview uses when the request carries no `?max_chars=`. Shown so the
    /// operator does not mistake it for the client's actual
    /// `[briefing] max_chars`, which the server has no way to see.
    pub orcamento_padrao: usize,
    /// The brief rendered by this crate's markdown renderer (HTML, trusted).
    pub html: String,
    /// The raw brief markdown, escaped by the template.
    pub markdown: String,
    /// Core pages the store returned.
    pub itens: Vec<ItemDoBriefing>,
}

// ---------------------------------------------------------------------------
// painel_timeline.html
// ---------------------------------------------------------------------------

/// One current page a timeline session produced, ready for the template.
pub(crate) struct ProducedPageRow {
    /// Relative wiki path.
    pub path: String,
    /// Page title.
    pub title: String,
    /// Semantic kind badge text (`rule`, `decision`, `concept`, `gotcha`, …).
    pub kind: String,
    /// Link target for this page.
    pub href: String,
}

/// One session on a project's timeline, grouped under its day.
pub(crate) struct TimelineSessionRow {
    /// Which agent CLI ran this session.
    pub agent: String,
    /// Duration label — `"Xh YYmin"`/`"Xmin"`/`"Xs"`, or `"em aberto"` while
    /// the session has no `ended_us`.
    pub duration_label: String,
    /// Observation-count label — the count, or `"—"` for an open session.
    pub observations_label: String,
    /// Current pages this session produced or reaffirmed.
    pub produced: Vec<ProducedPageRow>,
}

/// One UTC calendar day on the timeline: its sessions, and the bar width
/// (as a percentage of the day with the most sessions in the window).
pub(crate) struct TimelineDay {
    /// `YYYY-MM-DD`, UTC.
    pub date: String,
    /// Number of sessions that started this day.
    pub count: usize,
    /// `count * 100 / max_sessions_in_period`, capped at 100 — drives the
    /// inline `width` of the day's bar (no dynamic Tailwind class).
    pub pct: usize,
    /// This day's sessions, most recent first (inherited from the store's
    /// ordering).
    pub sessions: Vec<TimelineSessionRow>,
}

/// View-model for `GET /w/:workspace/:project/linha-do-tempo`.
#[derive(Template)]
#[template(path = "painel_timeline.html")]
pub(crate) struct TimelineView {
    /// Workspace name.
    pub workspace: String,
    /// Project name.
    pub project: String,
    /// Link target for this project, used by `_abas.html`.
    pub base_href: String,
    /// Active panel-tab for `_abas.html`.
    pub aba: &'static str,
    /// Effective `?dias=` window (7, 30, or 90 — never anything else).
    pub dias: i64,
    /// Days with at least one session in the window, most recent first.
    pub days: Vec<TimelineDay>,
}

// ---------------------------------------------------------------------------
// painel_proposals.html
// ---------------------------------------------------------------------------

/// One evidence quote cited by a proposal, ready for the template.
pub(crate) struct EvidenceRow {
    /// Source label the reviewer cited (e.g. `sessions/<id>.md`), escaped
    /// by askama like any other untrusted text.
    pub page: String,
    /// The quoted excerpt, escaped by askama.
    pub quote: String,
    /// Link target when `page` names a session capture, built through
    /// `page_href` (percent-encoded) — `None` otherwise, so the template
    /// never interpolates raw text into an `href`.
    pub href: Option<String>,
}

/// One pending (or otherwise filtered) auto-improvement proposal, ready for
/// the template. The web never approves or rejects; `approve_cmd`/
/// `reject_cmd` are the exact CLI commands shown as selectable text.
pub(crate) struct ProposalCard {
    /// Proposal id, as its canonical string form.
    pub id: String,
    /// Human-readable proposal title (untrusted — reviewer/LLM output).
    pub title: String,
    /// Proposal category for telemetry (e.g. `learning`).
    pub kind: String,
    /// `"create"` or `"update"`.
    pub operation: &'static str,
    /// Wiki path of the targeted page.
    pub target_path: String,
    /// Reviewer confidence as a rounded percentage (`0..=100`).
    pub confidence_pct: i64,
    /// Whether the target page changed since staging (`update` only —
    /// `sha256(current body) != target_body_sha256_at_stage`, or the page
    /// vanished entirely).
    pub conflict: bool,
    /// Why the reviewer proposes this edit (untrusted).
    pub rationale: String,
    /// Supporting evidence, with session links resolved.
    pub evidence: Vec<EvidenceRow>,
    /// Current body of the target page, for `update` proposals — `None`
    /// for `create` (nothing to show) and for a vanished target.
    pub before: Option<String>,
    /// Full proposed page body (raw markdown, escaped by askama — no
    /// markdown rendering here, so no new `|safe` surface).
    pub after: String,
    /// Exact, selectable `ai-memory pending-writes approve <ID>` command.
    pub approve_cmd: String,
    /// Exact, selectable `ai-memory pending-writes reject <ID>` command.
    pub reject_cmd: String,
}

/// View-model for `GET /w/:workspace/:project/propostas`.
#[derive(Template)]
#[template(path = "painel_proposals.html")]
pub(crate) struct ProposalsView {
    /// Workspace name.
    pub workspace: String,
    /// Project name.
    pub project: String,
    /// Link target for this project, used by `_abas.html`.
    pub base_href: String,
    /// Active panel-tab for `_abas.html`.
    pub aba: &'static str,
    /// Effective `?status=` filter (`pending`, `approved`, `rejected`,
    /// `conflict`, or `failed` — never anything else).
    pub status: &'static str,
    /// Proposals matching `status`, most recently staged first (inherited
    /// from the store's ordering).
    pub proposals: Vec<ProposalCard>,
}

/// View-model for a namespace (directory) listing — `GET
/// /w/:workspace/:project/p/:namespace/` when the path names a namespace
/// rather than a page (#603).
#[derive(Template)]
#[template(path = "namespace.html")]
pub(crate) struct NamespaceView {
    /// Workspace name.
    pub workspace: String,
    /// Project name.
    pub project: String,
    /// Link back to the project overview.
    pub project_href: String,
    /// The namespace path (no trailing slash), e.g. `_lint` or `concepts`.
    pub namespace: String,
    /// Pages under this namespace.
    pub pages: Vec<PageRow>,
}

// ---------------------------------------------------------------------------
// painel_cross_project.html
// ---------------------------------------------------------------------------

/// One rule page inside a [`RuleGroupRow`], ready for the template.
pub(crate) struct RuleMemberRow {
    /// Owning project's name.
    pub project: String,
    /// Rule title (untrusted — page content).
    pub title: String,
    /// Wiki path of the rule page.
    pub path: String,
    /// Link target for this page, built through `page_href`.
    pub href: String,
}

/// A set of rule pages from at least two projects of the same workspace
/// judged to say the same thing ([`ai_memory_store::group_rules`]).
pub(crate) struct RuleGroupRow {
    /// Owning workspace's name (grouping never crosses workspaces).
    pub workspace: String,
    /// Member rules, sorted by `(project, path)` (inherited from `group_rules`).
    pub members: Vec<RuleMemberRow>,
}

/// One open handoff on the cross-project screen.
pub(crate) struct OpenHandoffRow {
    /// Owning workspace's name.
    pub workspace: String,
    /// Owning project's name (handoff destination).
    pub project: String,
    /// Link target for the owning project.
    pub project_href: String,
    /// Agent CLI that composed it.
    pub agent: String,
    /// Prompt-derived summary, withheld exactly like the JSON API withholds
    /// it (`serves_handoff_body`) — `None` when this caller may not read it,
    /// which the template renders as a "hidden, sign in to read" note.
    pub summary: Option<String>,
    /// Humanised age (e.g. "3 hours ago").
    pub age: String,
}

/// One pending cross-project message on the cross-project screen. Messages
/// carry no owner filter (see `painel_cross_project::collect_handoffs_and_messages`
/// doc comment) — every pending message addressed to a listed project is
/// shown regardless of the requesting actor.
pub(crate) struct PendingMessageRow {
    /// Sender workspace's name.
    pub from_workspace: String,
    /// Sender project's name.
    pub from_project: String,
    /// Recipient workspace's name.
    pub to_workspace: String,
    /// Recipient project's name.
    pub to_project: String,
    /// Subject when present and non-blank, else the message body
    /// (untrusted — composed by another project's agent; escaped by askama,
    /// never rendered as markdown or HTML).
    pub summary: String,
    /// Humanised age (e.g. "3 hours ago").
    pub age: String,
}

/// View-model for `GET /entre-projetos`.
#[derive(Template)]
#[template(path = "painel_cross_project.html")]
pub(crate) struct CrossProjectView {
    /// Rule groups spanning two or more projects, across every workspace.
    pub rule_groups: Vec<RuleGroupRow>,
    /// Open handoffs across every project the home page lists, scoped by the
    /// requesting actor's `OwnerFilter`.
    pub open_handoffs: Vec<OpenHandoffRow>,
    /// Pending cross-project messages across every project's inbox.
    pub pending_messages: Vec<PendingMessageRow>,
}

// ---------------------------------------------------------------------------
// page.html
// ---------------------------------------------------------------------------

/// View-model for `GET /w/:workspace/:project/p/*path`.
#[derive(Template)]
#[template(path = "page.html")]
pub(crate) struct PageView {
    /// Workspace name.
    pub workspace: String,
    /// Project name.
    pub project: String,
    /// Link target for the containing project.
    pub project_href: String,
    /// Relative wiki path.
    pub path: String,
    /// Page title.
    pub title: String,
    /// Semantic kind.
    pub kind: String,
    /// Memory tier.
    pub tier: String,
    /// Whether the page is pinned.
    pub pinned: bool,
    /// Humanised updated timestamp.
    pub updated_relative: String,
    /// Humanised created timestamp.
    pub created_relative: String,
    /// Path of the page this supersedes, or empty string.
    pub supersedes_path: String,
    /// Link target for the superseded page, or empty string.
    pub supersedes_href: String,
    /// Rendered markdown body (HTML, trusted).
    pub body_html: String,
    /// Username of the page's last author. Empty string when the page
    /// was authored anonymously / by root / pre-multi-user — the
    /// template uses the empty check to omit the "Last edited by"
    /// chip entirely, so legacy pages render with the exact same
    /// chrome they had before v0.8.
    pub author_username: String,
    /// Optional display name shown in parens after the username
    /// (e.g. `alice (Alice Smith)`). Empty when not set on the user row.
    pub author_name: String,
    /// Optional email rendered as a `mailto:` link after the username.
    /// Empty when not set on the user row.
    pub author_email: String,
}

// ---------------------------------------------------------------------------
// search.html
// ---------------------------------------------------------------------------

/// One FTS5 search hit.
pub(crate) struct SearchHit {
    /// Workspace name.
    pub workspace: String,
    /// Project name.
    pub project: String,
    /// Relative wiki path.
    pub path: String,
    /// Link target for this hit.
    pub href: String,
    /// Page title.
    pub title: String,
    /// FTS5 snippet (HTML-marked with `<mark>` tags).
    pub snippet: String,
}

/// View-model for `GET /search?q=…`.
#[derive(Template)]
#[template(path = "search.html")]
pub(crate) struct SearchView {
    /// The raw query string.
    pub query: String,
    /// FTS5 search hits.
    pub hits: Vec<SearchHit>,
    /// Pre-computed hit count for display (avoids needing `|length` filter).
    pub hit_count: usize,
}

// ---------------------------------------------------------------------------
// not_found.html
// ---------------------------------------------------------------------------

/// View-model for 404 responses.
#[derive(Template)]
#[template(path = "not_found.html")]
pub(crate) struct NotFoundView {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn href_helpers_percent_encode_segments() {
        // Relative (no leading slash) so they resolve against the injected
        // `<base href>` — see `project_href` docs.
        assert_eq!(
            project_href("default space", "proj#one"),
            "w/default%20space/proj%23one"
        );
        assert_eq!(
            page_href("default", "scratch", "notes/a b%25.md"),
            "w/default/scratch/p/notes/a%20b%2525.md"
        );
    }
}

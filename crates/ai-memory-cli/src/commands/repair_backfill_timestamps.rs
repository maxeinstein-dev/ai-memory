//! `ai-memory repair-backfill-timestamps` — fork-only offline repair of
//! `sessions.started_at`/`ended_at` for sessions `backfill` imported before it
//! carried the transcript's original event time (`occurred_at`).
//!
//! Reads the operator's local harness transcripts read-only, matches each one
//! to a session in the store purely by session id (the native id, or its
//! UUID v5 when the native id is not itself a UUID — the same rule
//! `resolve_native_session_id` in `ai-memory-hooks` uses), and rewrites the
//! matched session's `started_at`/`ended_at` to the transcript's first and
//! last event timestamps. Dry-run by default; `--apply` writes through the
//! `WriterHandle`, one transaction per session, and only after confirming no
//! sibling `ai-memory` process is alive (the same guard `reindex` uses).
//!
//! Does **not** touch `observations`, pages, or anything else, and does not
//! reimport or reconsolidate — it corrects the two columns the timeline
//! screen reads. Take a backup first: `ai-memory backup`, with the server
//! stopped, before `--apply`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use ai_memory_core::SessionId;
use ai_memory_store::Store;

use crate::cli::RepairBackfillTimestampsArgs;
use crate::config::Config;
use crate::process_guard::{busy_message, sibling_processes};

/// A transcript file larger than this per-line length is almost certainly not
/// the newline-delimited JSON this command expects; skip the line rather than
/// fail the whole scan on one malformed file.
const MAX_LINE_BYTES: usize = 128 * 1024;

/// Stop scanning after this many `.jsonl` files, so a misconfigured
/// `--transcripts-dir` (e.g. pointed at a much larger tree) cannot make the
/// command hang.
const MAX_SCAN_FILES: usize = 50_000;

/// Reject a candidate time more than this far in the future relative to "now"
/// — clock-skew margin, not a real correction target. Applies independently
/// to the computed start and end: a rejected end simply leaves the existing
/// `ended_at` alone rather than aborting the whole session's repair.
const FUTURE_SLACK_US: i64 = 5 * 60 * 1_000_000;

/// One session row read from the DB, decoupled from store types so
/// [`plan_repair`] is testable without a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionRow {
    pub id: String,
    pub started_us: i64,
    pub ended_us: Option<i64>,
}

/// One session's computed repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedRepair {
    pub session_id: String,
    pub old_started_us: i64,
    pub old_ended_us: Option<i64>,
    pub new_started_us: i64,
    /// The value to *write* for `ended_at`. `None` means "leave the column
    /// alone": either the session is still open (`old_ended_us` is `None`
    /// and must stay that way), or the transcript's own end time was
    /// rejected as being in the future, in which case the existing value
    /// survives untouched rather than being cleared.
    pub new_ended_us: Option<i64>,
}

/// The result of matching every DB session against the transcripts found
/// under one root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RepairPlan {
    /// Sessions with a matching transcript that carried at least one
    /// timestamp and did not land in the future.
    pub matched: Vec<PlannedRepair>,
    /// Session ids with no matching transcript under the search root.
    pub unmatched: Vec<String>,
    /// Session ids whose transcript was found but carried no `timestamp`
    /// field on any line.
    pub skipped_no_timestamps: Vec<String>,
    /// Session ids whose transcript's own start time was more than
    /// [`FUTURE_SLACK_US`] ahead of "now" — never moved into the future.
    pub skipped_future: Vec<String>,
}

/// Compute the repair plan for `sessions` against transcripts under
/// `transcripts_dir`. Pure and side-effect free: no I/O beyond reading the
/// transcript files, no clock reads (`now_us` is supplied by the caller so
/// this stays deterministic in tests).
pub(crate) fn plan_repair(
    transcripts_dir: &Path,
    sessions: &[SessionRow],
    now_us: i64,
) -> RepairPlan {
    let by_id = scan_transcripts(transcripts_dir);
    let mut plan = RepairPlan::default();
    for session in sessions {
        match by_id.get(&session.id) {
            None => plan.unmatched.push(session.id.clone()),
            Some(TranscriptTimes::NoTimestamps) => {
                plan.skipped_no_timestamps.push(session.id.clone());
            }
            Some(TranscriptTimes::Found { first_us, last_us }) => {
                if *first_us > now_us + FUTURE_SLACK_US {
                    plan.skipped_future.push(session.id.clone());
                    continue;
                }
                let new_ended_us = match session.ended_us {
                    // Never set an end on a session the DB still has open.
                    None => None,
                    Some(_) if *last_us <= now_us + FUTURE_SLACK_US => Some(*last_us),
                    // The transcript's end looks like it is in the future;
                    // leave the existing (already-closed) value alone.
                    Some(_) => None,
                };
                plan.matched.push(PlannedRepair {
                    session_id: session.id.clone(),
                    old_started_us: session.started_us,
                    old_ended_us: session.ended_us,
                    new_started_us: *first_us,
                    new_ended_us,
                });
            }
        }
    }
    plan
}

/// First/last `timestamp` found across every line of one transcript file, or
/// a marker that the file matched a session id but carried no timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptTimes {
    Found { first_us: i64, last_us: i64 },
    NoTimestamps,
}

fn merge_times(existing: &mut TranscriptTimes, new: TranscriptTimes) {
    if let (
        TranscriptTimes::Found {
            first_us: f1,
            last_us: l1,
        },
        TranscriptTimes::Found {
            first_us: f2,
            last_us: l2,
        },
    ) = (*existing, new)
    {
        *existing = TranscriptTimes::Found {
            first_us: f1.min(f2),
            last_us: l1.max(l2),
        };
        return;
    }
    if matches!(existing, TranscriptTimes::NoTimestamps)
        && matches!(new, TranscriptTimes::Found { .. })
    {
        *existing = new;
    }
}

/// Recursively collect every `.jsonl` transcript under `root` and index it by
/// resolved session id, regardless of which subfolder it lives in — the
/// project may have been merged from more than one cwd-encoded folder name,
/// so matching happens purely on session id (spec §4).
fn scan_transcripts(root: &Path) -> HashMap<String, TranscriptTimes> {
    let mut out: HashMap<String, TranscriptTimes> = HashMap::new();
    let mut pending = vec![root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            if !file_type.is_file()
                || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
            {
                continue;
            }
            scanned += 1;
            if scanned > MAX_SCAN_FILES {
                return out;
            }
            if let Some((session_id, times)) = scan_one_transcript(&path) {
                out.entry(session_id)
                    .and_modify(|existing| merge_times(existing, times))
                    .or_insert(times);
            }
        }
    }
    out
}

/// Read one transcript file (opened read-only) line by line, extracting the
/// `sessionId` header field and every line's `timestamp`. Returns `None` when
/// no line carries a `sessionId` at all (not a session transcript this
/// command recognizes); otherwise the resolved session id, paired with the
/// timestamp span or a "no timestamps found" marker.
fn scan_one_transcript(path: &Path) -> Option<(String, TranscriptTimes)> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut session_id: Option<String> = None;
    let mut first_us: Option<i64> = None;
    let mut last_us: Option<i64> = None;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if session_id.is_none()
            && let Some(raw) = value.get("sessionId").and_then(|v| v.as_str())
        {
            session_id = Some(resolve_transcript_session_id(raw));
        }
        if let Some(us) = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<jiff::Timestamp>().ok())
            .map(|t| t.as_microsecond())
        {
            first_us = Some(first_us.map_or(us, |f: i64| f.min(us)));
            last_us = Some(last_us.map_or(us, |l: i64| l.max(us)));
        }
    }
    let session_id = session_id?;
    let times = match (first_us, last_us) {
        (Some(first_us), Some(last_us)) => TranscriptTimes::Found { first_us, last_us },
        _ => TranscriptTimes::NoTimestamps,
    };
    Some((session_id, times))
}

/// Same rule as `resolve_native_session_id` in `ai-memory-hooks/src/router.rs`:
/// a UUID native id is used as-is (canonicalized); any other string is hashed
/// to a deterministic UUID v5, so hook capture and this offline repair agree
/// on one session key for a harness whose native id is not itself a UUID
/// (e.g. Codex, OpenCode).
fn resolve_transcript_session_id(raw: &str) -> String {
    match Uuid::parse_str(raw) {
        Ok(uuid) => uuid.to_string(),
        Err(_) => Uuid::new_v5(&Uuid::NAMESPACE_OID, raw.as_bytes()).to_string(),
    }
}

/// Whether `--apply` must be refused given the sibling `ai-memory` processes
/// found alive. Split out from [`run`] so it is testable with a fake PID list
/// instead of the real `sysinfo` scan, which `sibling_processes()` itself
/// short-circuits to empty under `cfg!(test)`.
fn refuse_apply_when_busy(apply: bool, siblings: &[sysinfo::Pid]) -> Option<String> {
    if apply && !siblings.is_empty() {
        Some(busy_message("repair-backfill-timestamps --apply", siblings))
    } else {
        None
    }
}

fn format_us(us: i64) -> String {
    jiff::Timestamp::from_microsecond(us)
        .map(|t| t.to_string())
        .unwrap_or_else(|_| us.to_string())
}

fn print_report(workspace: &str, project: &str, plan: &RepairPlan, apply: bool) {
    println!(
        "ai-memory: repair-backfill-timestamps for {workspace}/{project}: {} session(s) matched, \
         {} unmatched (no transcript found), {} skipped (transcript has no timestamps), {} skipped \
         (would move into the future).",
        plan.matched.len(),
        plan.unmatched.len(),
        plan.skipped_no_timestamps.len(),
        plan.skipped_future.len(),
    );
    if !plan.matched.is_empty() {
        let before_min = plan.matched.iter().map(|r| r.old_started_us).min().unwrap();
        let before_max = plan.matched.iter().map(|r| r.old_started_us).max().unwrap();
        let after_min = plan.matched.iter().map(|r| r.new_started_us).min().unwrap();
        let after_max = plan.matched.iter().map(|r| r.new_started_us).max().unwrap();
        println!(
            "  date range before: {} .. {}",
            format_us(before_min),
            format_us(before_max)
        );
        println!(
            "  date range after:  {} .. {}",
            format_us(after_min),
            format_us(after_max)
        );
    }
    if !apply {
        println!(
            "  dry run: nothing was written. Take a backup (`ai-memory backup`) with the server \
             stopped, then re-run with --apply."
        );
    }
}

/// Run the `repair-backfill-timestamps` subcommand.
///
/// # Errors
/// Returns an error when `--apply` is refused because another `ai-memory`
/// process is alive, the scope does not resolve, the store cannot be opened,
/// or a write fails.
pub async fn run(config: &Config, args: RepairBackfillTimestampsArgs) -> Result<()> {
    if let Some(message) = refuse_apply_when_busy(args.apply, &sibling_processes()) {
        bail!(message);
    }

    let (workspace, project) = super::resolve_scope(
        config,
        args.workspace.as_deref(),
        Some(args.project.as_str()),
    )?;

    let store =
        Store::open(&config.data_dir).context("opening store for repair-backfill-timestamps")?;
    let scope = ai_memory_store::lookup_existing_scope(&store.reader, &workspace, &project)
        .await
        .with_context(|| format!("resolving scope {workspace}/{project}"))?;

    let transcripts_dir = match &args.transcripts_dir {
        Some(dir) => dir.clone(),
        None => default_transcripts_dir(config)?,
    };

    let raw_sessions = store
        .reader
        .session_times_for_scope(scope.workspace_id, scope.project_id)
        .await
        .context("listing sessions to repair")?;
    let sessions: Vec<SessionRow> = raw_sessions
        .iter()
        .map(|s| SessionRow {
            id: s.session_id.to_string(),
            started_us: s.started_us,
            ended_us: s.ended_us,
        })
        .collect();

    let now_us = jiff::Timestamp::now().as_microsecond();
    let plan = plan_repair(&transcripts_dir, &sessions, now_us);
    print_report(&workspace, &project, &plan, args.apply);

    if !args.apply {
        return Ok(());
    }

    for repair in &plan.matched {
        let session_id = SessionId::from_str(&repair.session_id)
            .with_context(|| format!("parsing session id {}", repair.session_id))?;
        store
            .writer
            .set_session_times(
                scope.workspace_id,
                scope.project_id,
                session_id,
                repair.new_started_us,
                repair.new_ended_us,
            )
            .await
            .with_context(|| {
                format!("applying repaired times for session {}", repair.session_id)
            })?;
    }
    println!("applied {} repaired session(s).", plan.matched.len());
    Ok(())
}

/// The same discovery root `backfill` uses for Claude Code transcripts when
/// `--transcripts-dir` is not given (`ai-memory-workstream`'s
/// `session_root(ManagedHarness::Claude, home, None)`, i.e.
/// `<home>/.claude/projects`), duplicated here rather than exposed as new
/// public surface in that crate for one caller.
fn default_transcripts_dir(config: &Config) -> Result<PathBuf> {
    let home =
        super::run::native_home(config).context("locating the local harness session stores")?;
    Ok(home.join(".claude").join("projects"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_memory_core::{AgentKind, NewSession};
    use tempfile::TempDir;

    fn write_transcript(path: &Path, session_id: &str, first_ts: &str, last_ts: &str) {
        let mut body = String::new();
        body.push_str(
            &serde_json::json!({"sessionId": session_id, "timestamp": first_ts, "cwd": "/x"})
                .to_string(),
        );
        body.push('\n');
        body.push_str(
            &serde_json::json!({"sessionId": session_id, "timestamp": last_ts}).to_string(),
        );
        body.push('\n');
        fs::write(path, body).unwrap();
    }

    fn now_us() -> i64 {
        jiff::Timestamp::now().as_microsecond()
    }

    // --- plan_repair (pure) -------------------------------------------------

    #[test]
    fn plan_repair_matches_a_session_by_native_uuid_and_computes_first_last_timestamps() {
        let dir = TempDir::new().unwrap();
        let sid = "11111111-2222-3333-4444-555555555555";
        write_transcript(
            &dir.path().join("s.jsonl"),
            sid,
            "2026-09-10T12:00:00Z",
            "2026-09-10T12:05:00Z",
        );
        let sessions = vec![SessionRow {
            id: sid.to_string(),
            started_us: 0,
            ended_us: Some(1),
        }];
        let plan = plan_repair(dir.path(), &sessions, now_us());
        assert_eq!(plan.matched.len(), 1, "{plan:?}");
        let m = &plan.matched[0];
        assert_eq!(
            m.new_started_us,
            "2026-09-10T12:00:00Z"
                .parse::<jiff::Timestamp>()
                .unwrap()
                .as_microsecond()
        );
        assert_eq!(
            m.new_ended_us,
            Some(
                "2026-09-10T12:05:00Z"
                    .parse::<jiff::Timestamp>()
                    .unwrap()
                    .as_microsecond()
            )
        );
    }

    #[test]
    fn plan_repair_matches_a_non_uuid_native_id_by_its_uuid_v5() {
        let dir = TempDir::new().unwrap();
        let native = "codex-native-id-123";
        let resolved = Uuid::new_v5(&Uuid::NAMESPACE_OID, native.as_bytes()).to_string();
        write_transcript(
            &dir.path().join("s.jsonl"),
            native,
            "2026-09-10T12:00:00Z",
            "2026-09-10T12:05:00Z",
        );
        let sessions = vec![SessionRow {
            id: resolved.clone(),
            started_us: 0,
            ended_us: None,
        }];
        let plan = plan_repair(dir.path(), &sessions, now_us());
        assert_eq!(plan.matched.len(), 1, "{plan:?}");
        assert_eq!(plan.matched[0].session_id, resolved);
    }

    #[test]
    fn plan_repair_matches_regardless_of_which_subfolder_the_transcript_lives_in() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("some-other-encoded-cwd-folder");
        fs::create_dir_all(&sub).unwrap();
        let sid = "44444444-5555-6666-7777-888888888888";
        write_transcript(
            &sub.join("s.jsonl"),
            sid,
            "2026-09-10T12:00:00Z",
            "2026-09-10T12:05:00Z",
        );
        let sessions = vec![SessionRow {
            id: sid.to_string(),
            started_us: 0,
            ended_us: None,
        }];
        let plan = plan_repair(dir.path(), &sessions, now_us());
        assert_eq!(plan.matched.len(), 1, "{plan:?}");
    }

    #[test]
    fn plan_repair_reports_a_session_with_no_matching_transcript_as_unmatched() {
        let dir = TempDir::new().unwrap();
        let sessions = vec![SessionRow {
            id: "nope".to_string(),
            started_us: 0,
            ended_us: None,
        }];
        let plan = plan_repair(dir.path(), &sessions, now_us());
        assert_eq!(plan.unmatched, vec!["nope".to_string()]);
        assert!(plan.matched.is_empty());
    }

    #[test]
    fn plan_repair_skips_a_session_whose_transcript_has_no_timestamps() {
        let dir = TempDir::new().unwrap();
        let sid = "22222222-3333-4444-5555-666666666666";
        fs::write(
            dir.path().join("s.jsonl"),
            format!("{}\n", serde_json::json!({"sessionId": sid, "cwd": "/x"})),
        )
        .unwrap();
        let sessions = vec![SessionRow {
            id: sid.to_string(),
            started_us: 0,
            ended_us: None,
        }];
        let plan = plan_repair(dir.path(), &sessions, now_us());
        assert_eq!(plan.skipped_no_timestamps, vec![sid.to_string()]);
        assert!(plan.matched.is_empty());
    }

    #[test]
    fn plan_repair_never_sets_ended_at_for_an_open_session() {
        let dir = TempDir::new().unwrap();
        let sid = "33333333-4444-5555-6666-777777777777";
        write_transcript(
            &dir.path().join("s.jsonl"),
            sid,
            "2026-09-10T12:00:00Z",
            "2026-09-10T12:05:00Z",
        );
        let sessions = vec![SessionRow {
            id: sid.to_string(),
            started_us: 0,
            ended_us: None,
        }];
        let plan = plan_repair(dir.path(), &sessions, now_us());
        assert_eq!(plan.matched.len(), 1, "{plan:?}");
        assert_eq!(
            plan.matched[0].new_ended_us, None,
            "an open session must never get an end time"
        );
    }

    #[test]
    fn plan_repair_rejects_a_start_time_more_than_five_minutes_in_the_future() {
        let dir = TempDir::new().unwrap();
        let sid = "55555555-6666-7777-8888-999999999999";
        let now = now_us();
        let far_future = format_us(now + 60 * 60 * 1_000_000); // +1h
        write_transcript(&dir.path().join("s.jsonl"), sid, &far_future, &far_future);
        let sessions = vec![SessionRow {
            id: sid.to_string(),
            started_us: 0,
            ended_us: None,
        }];
        let plan = plan_repair(dir.path(), &sessions, now);
        assert_eq!(plan.skipped_future, vec![sid.to_string()]);
        assert!(plan.matched.is_empty());
    }

    // --- busy guard (decision function, not sysinfo) -------------------------

    #[test]
    fn apply_is_refused_only_when_busy() {
        let siblings = vec![sysinfo::Pid::from_u32(999_999)];
        assert!(refuse_apply_when_busy(true, &siblings).is_some());
        assert!(refuse_apply_when_busy(true, &[]).is_none());
        assert!(
            refuse_apply_when_busy(false, &siblings).is_none(),
            "dry-run is never blocked by a live sibling process"
        );
    }

    // --- run() (Store-backed) -------------------------------------------------

    fn args(project: &str, transcripts_dir: &Path, apply: bool) -> RepairBackfillTimestampsArgs {
        RepairBackfillTimestampsArgs {
            workspace: Some("default".to_string()),
            project: project.to_string(),
            transcripts_dir: Some(transcripts_dir.to_path_buf()),
            apply,
        }
    }

    #[tokio::test]
    async fn dry_run_does_not_write_to_the_store() {
        let tmp = TempDir::new().unwrap();
        let config = Config {
            data_dir: tmp.path().to_path_buf(),
            ..Config::default()
        };
        let store = Store::open(tmp.path()).unwrap();
        let ws = store
            .writer
            .get_or_create_workspace("default")
            .await
            .unwrap();
        let proj = store
            .writer
            .get_or_create_project(ws, "scratch", None)
            .await
            .unwrap();
        let sid = SessionId::new();
        let original = 1_700_000_000_000_000;
        store
            .writer
            .begin_session(NewSession {
                occurred_at: Some(original),
                id: sid,
                workspace_id: ws,
                project_id: proj,
                agent_kind: AgentKind::ClaudeCode,
                cwd: None,
                actor_user: None,
            })
            .await
            .unwrap();
        store.writer.end_session(sid, None).await.unwrap();
        drop(store);

        let transcripts = TempDir::new().unwrap();
        write_transcript(
            &transcripts.path().join("t.jsonl"),
            &sid.to_string(),
            "2026-09-10T12:00:00Z",
            "2026-09-10T12:05:00Z",
        );

        run(&config, args("scratch", transcripts.path(), false))
            .await
            .unwrap();

        let store = Store::open(tmp.path()).unwrap();
        let times = store
            .reader
            .session_times_for_scope(ws, proj)
            .await
            .unwrap();
        assert_eq!(times.len(), 1);
        assert_eq!(
            times[0].started_us, original,
            "dry run must not write anything"
        );
    }

    /// Adversarial: a DB with two projects whose sessions are both matchable
    /// by transcripts (a merged-folder scenario). `--project` must rewrite
    /// only the requested project's sessions; the other project is the
    /// control and must keep its original times.
    ///
    /// Bite-check performed manually: removing the `project_id` filter from
    /// `ReaderPool::session_times_for_scope`'s `WHERE` clause makes this test
    /// fail (proj-b's session gets listed and repaired too); restoring the
    /// filter makes it pass again.
    #[tokio::test]
    async fn apply_only_rewrites_sessions_of_the_requested_project() {
        let tmp = TempDir::new().unwrap();
        let config = Config {
            data_dir: tmp.path().to_path_buf(),
            ..Config::default()
        };
        let store = Store::open(tmp.path()).unwrap();
        let ws = store
            .writer
            .get_or_create_workspace("default")
            .await
            .unwrap();
        let proj_a = store
            .writer
            .get_or_create_project(ws, "proj-a", None)
            .await
            .unwrap();
        let proj_b = store
            .writer
            .get_or_create_project(ws, "proj-b", None)
            .await
            .unwrap();
        let sid_a = SessionId::new();
        let sid_b = SessionId::new();
        let original = 1_700_000_000_000_000;
        for (proj, sid) in [(proj_a, sid_a), (proj_b, sid_b)] {
            store
                .writer
                .begin_session(NewSession {
                    occurred_at: Some(original),
                    id: sid,
                    workspace_id: ws,
                    project_id: proj,
                    agent_kind: AgentKind::ClaudeCode,
                    cwd: None,
                    actor_user: None,
                })
                .await
                .unwrap();
            store.writer.end_session(sid, None).await.unwrap();
        }
        drop(store);

        let transcripts = TempDir::new().unwrap();
        write_transcript(
            &transcripts.path().join("a.jsonl"),
            &sid_a.to_string(),
            "2026-09-10T12:00:00Z",
            "2026-09-10T12:05:00Z",
        );
        write_transcript(
            &transcripts.path().join("b.jsonl"),
            &sid_b.to_string(),
            "2026-09-11T12:00:00Z",
            "2026-09-11T12:05:00Z",
        );

        run(&config, args("proj-a", transcripts.path(), true))
            .await
            .unwrap();

        let store = Store::open(tmp.path()).unwrap();
        let times_a = store
            .reader
            .session_times_for_scope(ws, proj_a)
            .await
            .unwrap();
        let times_b = store
            .reader
            .session_times_for_scope(ws, proj_b)
            .await
            .unwrap();
        assert_ne!(
            times_a[0].started_us, original,
            "proj-a's session must be repaired"
        );
        assert_eq!(
            times_b[0].started_us, original,
            "proj-b's session must be untouched (control)"
        );
    }
}

//! `ReaderPool::timeline` (fork, painel web): sessions of a project and the
//! current pages each one produced (`page_evidence`, `source_kind =
//! 'session'`). Read-only; every query is filtered by (workspace_id,
//! project_id) per the fork's inherited security requirements
//! (docs/alfama/specs/2026-09-24-painel-web-alfama-design.md §2.1).

use ai_memory_core::{
    AgentKind, NewPage, NewSession, PageEvidence, PageEvidenceKind, PagePath, ProjectId, SessionId,
    Tier, WorkspaceId,
};
use ai_memory_store::Store;

async fn seeded() -> (tempfile::TempDir, Store, WorkspaceId, ProjectId) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("default".to_string())
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "p".to_string(), None)
        .await
        .unwrap();
    (tmp, store, ws, proj)
}

fn page(ws: WorkspaceId, proj: ProjectId, path: &str, title: &str, body: &str) -> NewPage {
    NewPage {
        workspace_id: ws,
        project_id: proj,
        path: PagePath::new(path).unwrap(),
        title: title.to_owned(),
        body: body.to_owned(),
        tier: Tier::Semantic,
        // No explicit `kind` in the frontmatter: `page_kind_expr` falls back
        // to inferring it from the path (`gotchas/...` -> "gotcha"), which is
        // what the timeline test below asserts on.
        frontmatter_json: serde_json::json!({}),
        pinned: false,
        links: Vec::new(),
        author_id: None,
        expires_at: None,
        entities: Vec::new(),
        evidence: Vec::new(),
    }
}

async fn begun_session(store: &Store, ws: WorkspaceId, proj: ProjectId) -> SessionId {
    let session_id = SessionId::new();
    store
        .writer
        .begin_session(NewSession {
            occurred_at: None,
            id: session_id,
            workspace_id: ws,
            project_id: proj,
            agent_kind: AgentKind::ClaudeCode,
            cwd: None,
            actor_user: None,
        })
        .await
        .unwrap();
    session_id
}

async fn ended_session(store: &Store, ws: WorkspaceId, proj: ProjectId) -> SessionId {
    let session_id = begun_session(store, ws, proj).await;
    store.writer.end_session(session_id, None).await.unwrap();
    session_id
}

/// The happy path: one ended session, one page it produced (cited via
/// `page_evidence`), shows up with the page attached.
#[tokio::test]
async fn timeline_lists_sessions_and_what_each_one_produced() {
    let (_tmp, store, ws, proj) = seeded().await;
    let sid = ended_session(&store, ws, proj).await;

    let mut p = page(ws, proj, "gotchas/x.md", "Gotcha X", "corpo");
    p.evidence = vec![PageEvidence {
        kind: PageEvidenceKind::Session,
        source_id: sid.to_string(),
    }];
    store.writer.upsert_page(p).await.unwrap();

    let timeline = store.reader.timeline(ws, proj, 0).await.unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].id, sid.to_string());
    assert_eq!(timeline[0].produced.len(), 1);
    assert_eq!(timeline[0].produced[0].path, "gotchas/x.md");
    assert_eq!(timeline[0].produced[0].kind, "gotcha");
    // The session ended, so its observation watermark is a real count.
    assert_eq!(timeline[0].observations, Some(0));
}

/// An open session (never `end_session`ed) must still show up, with its
/// observation count reported as `None` rather than a misleading `0` — the
/// persisted `ended_observation_count` column is a `NOT NULL DEFAULT 0`
/// placeholder until the session actually ends.
#[tokio::test]
async fn open_session_reports_no_observation_count() {
    let (_tmp, store, ws, proj) = seeded().await;
    let sid = begun_session(&store, ws, proj).await;

    let timeline = store.reader.timeline(ws, proj, 0).await.unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].id, sid.to_string());
    assert_eq!(timeline[0].ended_us, None);
    assert_eq!(
        timeline[0].observations, None,
        "an open session's observation count is not yet meaningful"
    );
}

/// `since_us` is an inclusive lower bound on `started_at`: a session that
/// started before the window must not appear, even though it produced a page.
/// The session here starts at real "now"; the window is pushed into the
/// future so it falls outside on the near side, mirroring how
/// `session_counts_by_agent`'s own `since` test proves exclusion (a future
/// cutoff, not a backdated row — the reader pool is read-only).
#[tokio::test]
async fn session_outside_the_window_does_not_appear() {
    let (_tmp, store, ws, proj) = seeded().await;
    let sid = ended_session(&store, ws, proj).await;

    let mut p = page(ws, proj, "gotchas/x.md", "Gotcha X", "corpo");
    p.evidence = vec![PageEvidence {
        kind: PageEvidenceKind::Session,
        source_id: sid.to_string(),
    }];
    store.writer.upsert_page(p).await.unwrap();

    let since_us = jiff::Timestamp::now().as_microsecond() + 60_000_000;
    let timeline = store.reader.timeline(ws, proj, since_us).await.unwrap();
    assert!(
        timeline.is_empty(),
        "a window starting in the future excludes the session"
    );
}

/// A page superseded by a newer version of itself (no longer `is_latest`)
/// must not count as something the citing session "produced" today.
#[tokio::test]
async fn non_latest_page_does_not_enter_produced() {
    let (_tmp, store, ws, proj) = seeded().await;
    let sid = ended_session(&store, ws, proj).await;

    let mut p1 = page(ws, proj, "gotchas/x.md", "Gotcha X", "v1");
    p1.evidence = vec![PageEvidence {
        kind: PageEvidenceKind::Session,
        source_id: sid.to_string(),
    }];
    store.writer.upsert_page(p1).await.unwrap();
    // A second version (different body) supersedes the first; the first row
    // stops being `is_latest`, but its evidence row for `sid` stays behind.
    let p2 = page(ws, proj, "gotchas/x.md", "Gotcha X", "v2, no new evidence");
    store.writer.upsert_page(p2).await.unwrap();

    let timeline = store.reader.timeline(ws, proj, 0).await.unwrap();
    assert_eq!(timeline.len(), 1);
    assert!(
        timeline[0].produced.is_empty(),
        "the superseded version must not appear as produced"
    );
}

/// A page from another project must never leak into this project's timeline,
/// even if (hypothetically) it cited the same session id.
#[tokio::test]
async fn page_from_another_project_does_not_enter_produced() {
    let (_tmp, store, ws, proj) = seeded().await;
    let other_proj = store
        .writer
        .get_or_create_project(ws, "outro".to_string(), None)
        .await
        .unwrap();
    let sid = ended_session(&store, ws, proj).await;

    let mut p = page(ws, other_proj, "gotchas/y.md", "Gotcha Y", "corpo");
    p.evidence = vec![PageEvidence {
        kind: PageEvidenceKind::Session,
        source_id: sid.to_string(),
    }];
    store.writer.upsert_page(p).await.unwrap();

    let timeline = store.reader.timeline(ws, proj, 0).await.unwrap();
    assert_eq!(timeline.len(), 1);
    assert!(
        timeline[0].produced.is_empty(),
        "a page from another project must not appear"
    );
}

// --- Adversarial security-boundary tests (alfama-2, docs/security-boundaries.md) ---
//
// `timeline` is a read entry point scoped by `(workspace_id, project_id)`;
// per the fork's AGENTS.md rule, it is guilty of leaking until an adversarial
// test proves otherwise. Each test below attempts a specific violation and
// asserts refusal; `legitimate_session_appears_in_its_own_project_timeline`
// is the control case proving the guard is not just a blanket empty result.

/// A session that belongs to another project in the *same* workspace must
/// never appear on this project's timeline, even though its id is valid and
/// resolvable.
#[tokio::test]
async fn session_from_another_project_in_same_workspace_does_not_appear() {
    let (_tmp, store, ws, proj_a) = seeded().await;
    let proj_b = store
        .writer
        .get_or_create_project(ws, "b".to_string(), None)
        .await
        .unwrap();
    let sid_b = ended_session(&store, ws, proj_b).await;

    let timeline_a = store.reader.timeline(ws, proj_a, 0).await.unwrap();
    assert!(
        timeline_a.is_empty(),
        "a session from another project in the same workspace must not appear"
    );

    // Control: the same session appears on its own project's timeline.
    let timeline_b = store.reader.timeline(ws, proj_b, 0).await.unwrap();
    assert_eq!(timeline_b.len(), 1);
    assert_eq!(timeline_b[0].id, sid_b.to_string());
}

/// A session that belongs to a *different workspace* must not appear, even
/// when the project in that other workspace has the exact same name as the
/// project being queried (same-named projects in different workspaces are
/// distinct scope coordinates, per boundary #2 in `docs/security-boundaries.md`).
#[tokio::test]
async fn session_from_another_workspace_with_same_project_name_does_not_appear() {
    let (_tmp, store, ws_a, proj_a) = seeded().await;
    let ws_b = store
        .writer
        .get_or_create_workspace("other-workspace".to_string())
        .await
        .unwrap();
    let proj_b = store
        .writer
        .get_or_create_project(ws_b, "p".to_string(), None)
        .await
        .unwrap();
    let sid_b = ended_session(&store, ws_b, proj_b).await;

    let timeline_a = store.reader.timeline(ws_a, proj_a, 0).await.unwrap();
    assert!(
        timeline_a.is_empty(),
        "a session from another workspace must not appear, even with a same-named project"
    );

    // Control: the same session appears on its own workspace/project timeline.
    let timeline_b = store.reader.timeline(ws_b, proj_b, 0).await.unwrap();
    assert_eq!(timeline_b.len(), 1);
    assert_eq!(timeline_b[0].id, sid_b.to_string());
}

/// Evidence pointing at a session that belongs to another project must not
/// leak that session (or its evidence) into this project's timeline, even
/// though the evidence row itself lives on a page in this project.
#[tokio::test]
async fn evidence_pointing_to_a_session_of_another_project_does_not_leak() {
    let (_tmp, store, ws, proj_a) = seeded().await;
    let proj_b = store
        .writer
        .get_or_create_project(ws, "b".to_string(), None)
        .await
        .unwrap();
    let sid_b = ended_session(&store, ws, proj_b).await;

    // A page in project A cites, as evidence, a session that actually
    // belongs to project B.
    let mut p = page(ws, proj_a, "gotchas/x.md", "Gotcha X", "corpo");
    p.evidence = vec![PageEvidence {
        kind: PageEvidenceKind::Session,
        source_id: sid_b.to_string(),
    }];
    store.writer.upsert_page(p).await.unwrap();

    let timeline_a = store.reader.timeline(ws, proj_a, 0).await.unwrap();
    assert!(
        timeline_a.is_empty(),
        "a session belonging to another project must not appear on this project's \
         timeline just because a page here cites it as evidence"
    );
}

/// `since_us` cutoff excludes older sessions (the exclusion side of the
/// window boundary, adversarial form: a session that legitimately belongs to
/// this project but predates the window must still be excluded).
#[tokio::test]
async fn since_us_cutoff_excludes_older_sessions() {
    let (_tmp, store, ws, proj) = seeded().await;
    let _older = ended_session(&store, ws, proj).await;

    let far_future_cutoff = jiff::Timestamp::now().as_microsecond() + 60_000_000;
    let timeline = store
        .reader
        .timeline(ws, proj, far_future_cutoff)
        .await
        .unwrap();
    assert!(
        timeline.is_empty(),
        "a session started before the cutoff must be excluded"
    );

    // Control: with a cutoff at (or before) the epoch, the same session
    // legitimately appears.
    let timeline = store.reader.timeline(ws, proj, 0).await.unwrap();
    assert_eq!(timeline.len(), 1);
}

/// Legitimate control case: a session that genuinely belongs to the queried
/// `(workspace_id, project_id)` appears on its own timeline.
#[tokio::test]
async fn legitimate_session_appears_in_its_own_project_timeline() {
    let (_tmp, store, ws, proj) = seeded().await;
    let sid = ended_session(&store, ws, proj).await;

    let timeline = store.reader.timeline(ws, proj, 0).await.unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].id, sid.to_string());
}

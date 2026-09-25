//! `ReaderPool::timeline` and `ReaderPool::project_overview` (fork, painel
//! web): sessions of a project and the current pages each one produced
//! (`page_evidence`, `source_kind = 'session'`). Read-only; every query is
//! filtered by (workspace_id, project_id) per the fork's inherited security
//! requirements (docs/alfama/specs/2026-09-24-painel-web-alfama-design.md
//! §2.1).

use std::collections::HashMap;

use ai_memory_core::{
    AgentKind, NewPage, NewSession, PageEvidence, PageEvidenceKind, PagePath, ProjectId, SessionId,
    Tier, WorkspaceId,
};
use ai_memory_store::{
    OverviewPage, ProjectOverview, RuleCandidate, Store, f32_vec_to_bytes, group_rules,
    is_system_page, iso_week_key, origin_counts_by_day, summary_line, weekly_changes,
};

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

/// A `_rules/` page: `page_kind_expr` infers `kind = 'rule'` for it from the
/// path alone, same as the tests in `Tarefa 13` of the panel plan.
fn rule_page(ws: WorkspaceId, proj: ProjectId, slug: &str, title: &str) -> NewPage {
    page(
        ws,
        proj,
        &format!("_rules/{slug}.md"),
        title,
        "corpo da regra",
    )
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

/// Parse an RFC 3339 instant into microseconds since the epoch, for tests
/// that need to control a session's `started_at` precisely — origin-date
/// and ISO-week math both key off it.
fn micros(rfc3339: &str) -> i64 {
    rfc3339.parse::<jiff::Timestamp>().unwrap().as_microsecond()
}

/// A session whose `started_at` is pinned to `started_us`, via `NewSession`'s
/// `occurred_at` (backfill's own mechanism for a known event time) rather
/// than "now" — needed to build deterministic origin-date/ISO-week fixtures.
async fn session_started_at(
    store: &Store,
    ws: WorkspaceId,
    proj: ProjectId,
    started_us: i64,
) -> SessionId {
    let session_id = SessionId::new();
    store
        .writer
        .begin_session(NewSession {
            occurred_at: Some(started_us),
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

// --- `group_rules` (pure) + `ReaderPool::rules_across_projects` ---
//
// Rule pages that say the same thing, repeated across projects, are
// candidates for promotion to a shared/global rule (Tarefa 13, plan
// `docs/alfama/plans/2026-09-24-painel-web-alfama.md`, §Fase 3).

fn candidate(
    ws: WorkspaceId,
    project: &str,
    path: &str,
    title: &str,
    vector: Option<Vec<f32>>,
) -> RuleCandidate {
    RuleCandidate {
        workspace: ws,
        project: project.to_string(),
        path: path.to_string(),
        title: title.to_string(),
        vector,
    }
}

/// Similar rules (by cosine over their embeddings) in different projects
/// group together; a dissimilar rule does not join the group.
#[test]
fn similar_rules_across_projects_group_together() {
    let ws = WorkspaceId::new();
    let rules = vec![
        candidate(
            ws,
            "a",
            "_rules/env.md",
            "Nunca commitar .env",
            Some(vec![1.0, 0.0]),
        ),
        candidate(
            ws,
            "b",
            "_rules/env.md",
            "Nao versionar arquivos .env",
            Some(vec![0.99, 0.05]),
        ),
        candidate(ws, "c", "_rules/tabs.md", "Usar tabs", Some(vec![0.0, 1.0])),
    ];

    let groups = group_rules(rules, 0.85);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].projects(), vec!["a".to_string(), "b".to_string()]);
}

/// Without a vector, two rules in different projects still group when their
/// titles are equal modulo case, accents and whitespace.
#[test]
fn no_vector_groups_by_normalized_title() {
    let ws = WorkspaceId::new();
    let rules = vec![
        candidate(ws, "a", "_rules/x.md", "Nunca Commitar Segredos", None),
        candidate(ws, "b", "_rules/x.md", "nunca   commitar segredos", None),
    ];

    let groups = group_rules(rules, 0.85);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].projects(), vec!["a".to_string(), "b".to_string()]);
}

/// Two similar rules in the *same* project never form a group by
/// themselves — promotion candidates need at least two distinct projects.
#[test]
fn same_project_pair_alone_does_not_form_a_group() {
    let ws = WorkspaceId::new();
    let rules = vec![
        candidate(
            ws,
            "a",
            "_rules/x.md",
            "Nunca commitar .env",
            Some(vec![1.0, 0.0]),
        ),
        candidate(
            ws,
            "a",
            "_rules/y.md",
            "Nao versionar .env",
            Some(vec![0.99, 0.05]),
        ),
    ];

    let groups = group_rules(rules, 0.85);

    assert!(groups.is_empty());
}

/// A zero-norm vector has no defined direction; cosine must be guarded
/// against it rather than dividing by zero (which would panic or produce
/// `NaN`, and `NaN >= threshold` is always false anyway but the guard makes
/// the intent explicit and the behavior independent of float edge cases).
#[test]
fn zero_norm_vectors_never_link() {
    let ws = WorkspaceId::new();
    let rules = vec![
        candidate(ws, "a", "_rules/x.md", "Regra A", Some(vec![0.0, 0.0])),
        candidate(ws, "b", "_rules/y.md", "Regra B", Some(vec![0.0, 0.0])),
    ];

    let groups = group_rules(rules, 0.0);

    assert!(
        groups.is_empty(),
        "zero-norm vectors must never be judged similar, even at threshold 0.0"
    );
}

/// Vectors of different dims must never be compared, even when the
/// threshold would otherwise be trivially satisfied.
#[test]
fn vectors_of_different_dims_are_never_compared() {
    let ws = WorkspaceId::new();
    let rules = vec![
        candidate(ws, "a", "_rules/x.md", "Regra A", Some(vec![1.0, 0.0])),
        candidate(
            ws,
            "b",
            "_rules/y.md",
            "Regra B diferente",
            Some(vec![1.0, 0.0, 0.0]),
        ),
    ];

    let groups = group_rules(rules, 0.5);

    assert!(groups.is_empty());
}

/// `rules_across_projects` only attaches a vector when its
/// `(provider, model, dim)` triple is the workspace's most common one
/// (invariant 8); a page embedded under a different model is still
/// returned, but with `vector: None`.
#[tokio::test]
async fn rules_across_projects_ignores_vectors_of_a_non_majority_triple() {
    let (_tmp, store, ws, proj_p) = seeded().await;
    let proj_b = store
        .writer
        .get_or_create_project(ws, "b".to_string(), None)
        .await
        .unwrap();
    let proj_c = store
        .writer
        .get_or_create_project(ws, "c".to_string(), None)
        .await
        .unwrap();

    let p_id = store
        .writer
        .upsert_page(rule_page(ws, proj_p, "env", "Nunca commitar .env"))
        .await
        .unwrap();
    let b_id = store
        .writer
        .upsert_page(rule_page(ws, proj_b, "env", "Nao versionar .env"))
        .await
        .unwrap();
    let c_id = store
        .writer
        .upsert_page(rule_page(ws, proj_c, "env", "Nunca subir .env"))
        .await
        .unwrap();

    // Majority triple in this workspace: (mock, mock-embed, 2), two rows.
    // The minority triple (mock, old-embed, 2) has just one row.
    store
        .writer
        .store_embedding(
            p_id,
            f32_vec_to_bytes(&[1.0, 0.0]),
            "mock".into(),
            "mock-embed".into(),
            2,
        )
        .await
        .unwrap();
    store
        .writer
        .store_embedding(
            b_id,
            f32_vec_to_bytes(&[0.99, 0.05]),
            "mock".into(),
            "mock-embed".into(),
            2,
        )
        .await
        .unwrap();
    store
        .writer
        .store_embedding(
            c_id,
            f32_vec_to_bytes(&[0.0, 1.0]),
            "mock".into(),
            "old-embed".into(),
            2,
        )
        .await
        .unwrap();

    let candidates = store.reader.rules_across_projects(ws).await.unwrap();
    assert_eq!(candidates.len(), 3);
    let by_project: HashMap<String, RuleCandidate> = candidates
        .into_iter()
        .map(|c| (c.project.clone(), c))
        .collect();
    assert!(by_project["p"].vector.is_some());
    assert!(by_project["b"].vector.is_some());
    assert!(
        by_project["c"].vector.is_none(),
        "a vector from a non-majority (provider, model, dim) triple must be ignored"
    );
}

/// End-to-end: once a non-majority-triple vector is dropped to `None`,
/// grouping still finds the pair through the normalized-title fallback.
#[tokio::test]
async fn non_majority_model_vectors_fall_back_to_title_matching() {
    let (_tmp, store, ws, proj_p) = seeded().await;
    let proj_maj_a = store
        .writer
        .get_or_create_project(ws, "maj-a".to_string(), None)
        .await
        .unwrap();
    let proj_maj_b = store
        .writer
        .get_or_create_project(ws, "maj-b".to_string(), None)
        .await
        .unwrap();
    let proj_minor = store
        .writer
        .get_or_create_project(ws, "minor".to_string(), None)
        .await
        .unwrap();

    // Establish the majority triple with two unrelated rules elsewhere in
    // the workspace.
    let maj_a_id = store
        .writer
        .upsert_page(rule_page(ws, proj_maj_a, "other", "Outra regra"))
        .await
        .unwrap();
    let maj_b_id = store
        .writer
        .upsert_page(rule_page(ws, proj_maj_b, "other2", "Outra regra 2"))
        .await
        .unwrap();
    store
        .writer
        .store_embedding(
            maj_a_id,
            f32_vec_to_bytes(&[1.0, 0.0]),
            "mock".into(),
            "main".into(),
            2,
        )
        .await
        .unwrap();
    store
        .writer
        .store_embedding(
            maj_b_id,
            f32_vec_to_bytes(&[0.0, 1.0]),
            "mock".into(),
            "main".into(),
            2,
        )
        .await
        .unwrap();

    // Two rules with the same normalized title, both embedded under a
    // non-majority triple: their vectors must be ignored, so grouping can
    // only find them via the title fallback.
    let p_id = store
        .writer
        .upsert_page(rule_page(ws, proj_p, "env", "Nunca commitar .env"))
        .await
        .unwrap();
    let minor_id = store
        .writer
        .upsert_page(rule_page(ws, proj_minor, "env", "nunca   commitar .env"))
        .await
        .unwrap();
    store
        .writer
        .store_embedding(
            p_id,
            f32_vec_to_bytes(&[1.0, 0.0, 0.0]),
            "mock".into(),
            "stale".into(),
            3,
        )
        .await
        .unwrap();
    store
        .writer
        .store_embedding(
            minor_id,
            f32_vec_to_bytes(&[0.0, 0.0, 1.0]),
            "mock".into(),
            "stale".into(),
            3,
        )
        .await
        .unwrap();

    let candidates = store.reader.rules_across_projects(ws).await.unwrap();
    let by_project: HashMap<String, RuleCandidate> = candidates
        .into_iter()
        .map(|c| (c.project.clone(), c))
        .collect();
    assert!(by_project["p"].vector.is_none());
    assert!(by_project["minor"].vector.is_none());
    assert!(by_project["maj-a"].vector.is_some());

    let groups = group_rules(
        vec![by_project["p"].clone(), by_project["minor"].clone()],
        0.85,
    );
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].projects(),
        vec!["minor".to_string(), "p".to_string()]
    );
}

/// A malformed embedding blob (length not a multiple of 4) must decode to
/// `None` rather than panicking.
#[tokio::test]
async fn malformed_embedding_blob_becomes_none_without_panicking() {
    let (_tmp, store, ws, proj) = seeded().await;
    let id = store
        .writer
        .upsert_page(rule_page(ws, proj, "env", "Regra"))
        .await
        .unwrap();
    store
        .writer
        .store_embedding(id, vec![1, 2, 3, 4, 5], "mock".into(), "main".into(), 1)
        .await
        .unwrap();

    let candidates = store.reader.rules_across_projects(ws).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(
        candidates[0].vector.is_none(),
        "a malformed blob must decode to None, never panic"
    );
}

/// A blob whose length is a clean multiple of 4 but does not match the
/// stored `dim` must also decode to `None`, never panic and never be
/// silently truncated/padded.
#[tokio::test]
async fn embedding_length_mismatched_with_dim_becomes_none() {
    let (_tmp, store, ws, proj) = seeded().await;
    let id = store
        .writer
        .upsert_page(rule_page(ws, proj, "env", "Regra"))
        .await
        .unwrap();
    store
        .writer
        .store_embedding(
            id,
            f32_vec_to_bytes(&[1.0, 0.0]),
            "mock".into(),
            "main".into(),
            3,
        )
        .await
        .unwrap();

    let candidates = store.reader.rules_across_projects(ws).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].vector.is_none());
}

// --- Adversarial security-boundary test (alfama-4, docs/security-boundaries.md) ---
//
// `rules_across_projects` aggregates *across projects* by design (that is
// the point of the "Entre projetos" screen), but per the fork's AGENTS.md
// rule any entry point that fans out across projects is guilty of leaking
// across *workspaces* until an adversarial test proves otherwise.

/// Two equivalent rules in two different workspaces never appear together:
/// calling `rules_across_projects` for workspace A returns none of
/// workspace B's rules, so they can never be grouped. Control: within one
/// workspace, an equivalent rule pair across two of its own projects does
/// group.
#[tokio::test]
async fn rules_from_another_workspace_never_appear_or_group() {
    let (_tmp, store, ws_a, proj_a) = seeded().await;
    let ws_b = store
        .writer
        .get_or_create_workspace("outra-workspace".to_string())
        .await
        .unwrap();
    let proj_b = store
        .writer
        .get_or_create_project(ws_b, "p".to_string(), None)
        .await
        .unwrap();

    store
        .writer
        .upsert_page(rule_page(ws_a, proj_a, "env", "Nunca commitar .env"))
        .await
        .unwrap();
    store
        .writer
        .upsert_page(rule_page(ws_b, proj_b, "env", "Nunca commitar .env"))
        .await
        .unwrap();

    let candidates_a = store.reader.rules_across_projects(ws_a).await.unwrap();
    assert_eq!(
        candidates_a.len(),
        1,
        "workspace A must not see workspace B's equivalent rule"
    );
    assert_eq!(candidates_a[0].project, "p");

    let groups_a = group_rules(candidates_a, 0.85);
    assert!(
        groups_a.is_empty(),
        "workspace A's single project cannot form a cross-project group by itself, \
         which is exactly what stops it from ever grouping with workspace B's rule"
    );

    // Control: within workspace B alone, the same rule repeated in a
    // second project of that workspace DOES group.
    let proj_b2 = store
        .writer
        .get_or_create_project(ws_b, "q".to_string(), None)
        .await
        .unwrap();
    store
        .writer
        .upsert_page(rule_page(ws_b, proj_b2, "env", "nunca   commitar .env"))
        .await
        .unwrap();

    let candidates_b = store.reader.rules_across_projects(ws_b).await.unwrap();
    assert_eq!(candidates_b.len(), 2);
    let groups_b = group_rules(candidates_b, 0.85);
    assert_eq!(
        groups_b.len(),
        1,
        "control: within the same workspace, an equivalent rule across two \
         of its own projects does group"
    );
    assert_eq!(
        groups_b[0].projects(),
        vec!["p".to_string(), "q".to_string()]
    );
}

// --- `ReaderPool::project_overview` (Tarefa 1, plan
// docs/alfama/plans/2026-09-25-visao-geral.md) ---

/// The origin date of a page is the `MIN` of its evidence sessions' start
/// times, not the earliest-cited one, the latest-cited one, or `created_at`.
/// Also exercises `ProjectOverview.sessions`/`first_session_us`/
/// `last_session_us`, since the same fixture covers them cheaply.
#[tokio::test]
async fn origin_is_the_min_of_evidence_session_starts() {
    let (_tmp, store, ws, proj) = seeded().await;
    let early_us = micros("2026-08-01T10:00:00Z");
    let late_us = micros("2026-08-10T10:00:00Z");
    let sid_early = session_started_at(&store, ws, proj, early_us).await;
    let sid_late = session_started_at(&store, ws, proj, late_us).await;

    let mut p = page(ws, proj, "decisions/x.md", "Decisão X", "corpo");
    p.evidence = vec![
        PageEvidence {
            kind: PageEvidenceKind::Session,
            source_id: sid_late.to_string(),
        },
        PageEvidence {
            kind: PageEvidenceKind::Session,
            source_id: sid_early.to_string(),
        },
    ];
    store.writer.upsert_page(p).await.unwrap();

    let overview = store.reader.project_overview(ws, proj).await.unwrap();
    let found = overview
        .pages
        .iter()
        .find(|p| p.path == "decisions/x.md")
        .expect("page present");
    assert_eq!(
        found.origin_us,
        Some(early_us),
        "origin is the MIN of the evidence session starts, not the latest one"
    );
    assert_eq!(
        found.evidence_starts_us,
        vec![early_us, late_us],
        "evidence starts are sorted ascending"
    );

    assert_eq!(overview.first_session_us, Some(early_us));
    assert_eq!(overview.last_session_us, Some(late_us));

    let produced_for = |sid: SessionId| {
        overview
            .sessions
            .iter()
            .find(|s| s.0 == sid)
            .map(|s| s.3)
            .unwrap_or(0)
    };
    assert_eq!(produced_for(sid_early), 1);
    assert_eq!(produced_for(sid_late), 1);
}

/// A page with no session evidence at all has no origin date. It must never
/// fall back to `created_at`/`updated_at` — those are the date the LLM
/// generated the page, not the date of the knowledge (§1.3 of the design
/// spec); `OverviewPage` does not even carry those fields, so there is no
/// path back to them.
#[tokio::test]
async fn page_without_session_evidence_has_no_origin() {
    let (_tmp, store, ws, proj) = seeded().await;
    store
        .writer
        .upsert_page(page(
            ws,
            proj,
            "concepts/lonely.md",
            "Sozinho",
            "corpo sem evidencia",
        ))
        .await
        .unwrap();

    let overview = store.reader.project_overview(ws, proj).await.unwrap();
    let found = overview
        .pages
        .iter()
        .find(|p| p.path == "concepts/lonely.md")
        .expect("page present");
    assert_eq!(
        found.origin_us, None,
        "a page with no session evidence has no origin date"
    );
    assert!(found.evidence_starts_us.is_empty());
}

// --- Adversarial security-boundary test (alfama, docs/security-boundaries.md) ---
//
// `project_overview` resolves evidence through a map of *this scope's*
// sessions only (fact 4 of the plan); per the fork's AGENTS.md rule, that
// resolution is guilty of leaking across projects/workspaces until an
// adversarial test proves otherwise.

/// Evidence citing a session from another PROJECT or another WORKSPACE must
/// not count toward a page's origin date, even though the evidence row
/// lives on a page in the scope under test. Control: a third, legitimate
/// evidence entry on the SAME page, citing a session that really is in
/// scope, does count — proving the exclusion is selective, not a blanket
/// empty result. Both out-of-scope sessions are backdated *before* the
/// legitimate one, so a broken scope filter would leak through as an
/// earlier (wrong) `origin_us` rather than silently passing.
#[tokio::test]
async fn evidence_from_another_project_or_workspace_does_not_count_toward_origin() {
    let (_tmp, store, ws_a, proj_a) = seeded().await;
    let proj_b = store
        .writer
        .get_or_create_project(ws_a, "b".to_string(), None)
        .await
        .unwrap();
    let ws_c = store
        .writer
        .get_or_create_workspace("outra-workspace".to_string())
        .await
        .unwrap();
    let proj_c = store
        .writer
        .get_or_create_project(ws_c, "p".to_string(), None)
        .await
        .unwrap();

    let bad_project_us = micros("2026-01-01T00:00:00Z");
    let bad_workspace_us = micros("2026-01-02T00:00:00Z");
    let good_us = micros("2026-01-03T00:00:00Z");

    let sid_b = session_started_at(&store, ws_a, proj_b, bad_project_us).await;
    let sid_c = session_started_at(&store, ws_c, proj_c, bad_workspace_us).await;
    let sid_good = session_started_at(&store, ws_a, proj_a, good_us).await;

    let mut p = page(ws_a, proj_a, "decisions/x.md", "Decisão X", "corpo");
    p.evidence = vec![
        PageEvidence {
            kind: PageEvidenceKind::Session,
            source_id: sid_b.to_string(),
        },
        PageEvidence {
            kind: PageEvidenceKind::Session,
            source_id: sid_c.to_string(),
        },
        PageEvidence {
            kind: PageEvidenceKind::Session,
            source_id: sid_good.to_string(),
        },
    ];
    store.writer.upsert_page(p).await.unwrap();

    let overview = store.reader.project_overview(ws_a, proj_a).await.unwrap();
    let found = overview
        .pages
        .iter()
        .find(|p| p.path == "decisions/x.md")
        .expect("page present");

    assert_eq!(
        found.evidence_starts_us,
        vec![good_us],
        "only the same-scope evidence counts: {:?}",
        found.evidence_starts_us
    );
    assert_eq!(
        found.origin_us,
        Some(good_us),
        "origin must come from the in-scope session, not the earlier out-of-scope ones"
    );
}

/// `project_overview` excludes system pages (`is_system_page`, moved here
/// from `ai-memory-web`), the same rule the page tree uses to split
/// knowledge from machinery — but keeps `_rules/` pages, which are
/// knowledge.
#[tokio::test]
async fn project_overview_excludes_system_pages() {
    let (_tmp, store, ws, proj) = seeded().await;

    store
        .writer
        .upsert_page(page(ws, proj, "decisions/x.md", "Decisão X", "corpo"))
        .await
        .unwrap();
    store
        .writer
        .upsert_page(page(ws, proj, "sessions/2026-09-25.md", "Sessão", "corpo"))
        .await
        .unwrap();
    store
        .writer
        .upsert_page(page(ws, proj, "_meta.md", "Meta", "corpo"))
        .await
        .unwrap();
    store
        .writer
        .upsert_page(rule_page(ws, proj, "env", "Nunca commitar .env"))
        .await
        .unwrap();

    let overview = store.reader.project_overview(ws, proj).await.unwrap();
    let paths: Vec<&str> = overview.pages.iter().map(|p| p.path.as_str()).collect();
    assert!(paths.contains(&"decisions/x.md"));
    assert!(
        paths.contains(&"_rules/env.md"),
        "a _rules/ page is knowledge, not system: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.starts_with("sessions/")),
        "session summaries are system pages: {paths:?}"
    );
    assert!(
        !paths.contains(&"_meta.md"),
        "_meta.md is a system page: {paths:?}"
    );
}

/// Sanity check on the moved predicate itself, independent of the query
/// that uses it.
#[test]
fn is_system_page_keeps_rules_but_hides_sessions_and_meta() {
    assert!(!is_system_page("_rules/env.md"));
    assert!(!is_system_page("decisions/x.md"));
    assert!(is_system_page("sessions/2026-09-25.md"));
    assert!(is_system_page("_meta.md"));
    assert!(is_system_page("_slots/core.md"));
}

// --- `iso_week_key` (pure) ---

/// The ISO week groups correctly across a month AND year boundary at once:
/// 2026-12-31 (a Thursday) and 2027-01-01 (a Friday) are adjacent calendar
/// days in different Gregorian years, but the same ISO week — that week's
/// Thursday (which decides the ISO year) is 2026-12-31, so both land on ISO
/// week 53 of 2026, not week 1 of 2027.
#[test]
fn iso_week_key_groups_the_year_boundary_into_the_same_iso_week() {
    let dec_31_2026 = micros("2026-12-31T12:00:00Z");
    let jan_1_2027 = micros("2027-01-01T12:00:00Z");
    assert_eq!(iso_week_key(dec_31_2026), (2026, 53));
    assert_eq!(iso_week_key(jan_1_2027), (2026, 53));
}

// --- `summary_line` (pure) ---

/// A non-blank frontmatter `summary` wins over the body entirely.
#[test]
fn summary_line_prefers_a_non_blank_frontmatter_summary() {
    let fm = serde_json::json!({ "summary": "Resumo curado." });
    assert_eq!(
        summary_line(&fm, "corpo qualquer, ignorado"),
        Some("Resumo curado.".to_string())
    );
}

/// A blank or absent frontmatter summary falls back to the body's first
/// paragraph, skipping the title heading.
#[test]
fn summary_line_falls_back_to_the_first_paragraph_when_frontmatter_is_blank_or_absent() {
    let body = "# Título\n\nPrimeiro parágrafo de verdade.\n\nSegundo parágrafo, ignorado.\n";

    let blank = serde_json::json!({ "summary": "   " });
    assert_eq!(
        summary_line(&blank, body),
        Some("Primeiro parágrafo de verdade.".to_string())
    );

    let absent = serde_json::json!({});
    assert_eq!(
        summary_line(&absent, body),
        Some("Primeiro parágrafo de verdade.".to_string())
    );
}

/// A leading YAML frontmatter fence inside the body itself (defence in
/// depth — `NewPage::body` is documented to exclude it already) is skipped,
/// same as the title heading.
#[test]
fn summary_line_skips_a_leading_frontmatter_fence_in_the_body() {
    let fm = serde_json::json!({});
    let body = "---\nkind: nota\n---\n# Título\n\nParágrafo real depois do frontmatter.\n";
    assert_eq!(
        summary_line(&fm, body),
        Some("Parágrafo real depois do frontmatter.".to_string())
    );
}

/// A summary longer than 200 chars is cut on a `char` boundary (never
/// splitting a multi-byte UTF-8 codepoint) and marked with an ellipsis.
#[test]
fn summary_line_cuts_at_200_chars_on_a_utf8_char_boundary() {
    let fm = serde_json::json!({});
    let body = "café ".repeat(80);
    let out = summary_line(&fm, &body).expect("body has prose");
    assert_eq!(out.chars().count(), 200);
    assert!(
        out.ends_with('\u{2026}'),
        "a cut summary ends with an ellipsis: {out:?}"
    );
}

// --- `weekly_changes` (pure) ---

fn overview_page(
    path: &str,
    kind: &str,
    origin_us: Option<i64>,
    evidence_starts_us: Vec<i64>,
) -> OverviewPage {
    OverviewPage {
        path: path.to_string(),
        title: path.to_string(),
        kind: kind.to_string(),
        summary: None,
        origin_us,
        evidence_starts_us,
    }
}

/// A concept new this week, one updated this week (earlier origin, fresh
/// evidence), and one untouched this week must be told apart.
#[test]
fn weekly_changes_distinguishes_new_concepts_from_updated_ones() {
    let week1_us = micros("2026-01-05T10:00:00Z");
    let week2_us = micros("2026-01-12T10:00:00Z");
    let sid1 = SessionId::new();
    let sid2 = SessionId::new();

    let overview = ProjectOverview {
        pages: vec![
            overview_page("concepts/new.md", "concept", Some(week2_us), vec![week2_us]),
            overview_page(
                "concepts/updated.md",
                "concept",
                Some(week1_us),
                vec![week1_us, week2_us],
            ),
            overview_page(
                "concepts/untouched.md",
                "concept",
                Some(week1_us),
                vec![week1_us],
            ),
        ],
        sessions: vec![
            (sid1, week1_us, "claude-code".to_string(), 1),
            (sid2, week2_us, "claude-code".to_string(), 2),
        ],
        first_session_us: Some(week1_us),
        last_session_us: Some(week2_us),
    };

    let weeks = weekly_changes(&overview, 4);
    let (year1, week1) = iso_week_key(week1_us);
    let wk1 = weeks
        .iter()
        .find(|w| w.year == year1 && w.week == week1)
        .expect("week1 present");
    // 2026-01-05 is itself a Monday, so it IS the start of its own ISO week.
    assert_eq!(wk1.start_date, "2026-01-05");

    let (year2, week2) = iso_week_key(week2_us);
    let wk2 = weeks
        .iter()
        .find(|w| w.year == year2 && w.week == week2)
        .expect("week2 present");

    let paths: Vec<&str> = wk2
        .new_or_updated_concepts
        .iter()
        .map(|p| p.path.as_str())
        .collect();
    assert!(paths.contains(&"concepts/new.md"), "{paths:?}");
    assert!(paths.contains(&"concepts/updated.md"), "{paths:?}");
    assert!(
        !paths.contains(&"concepts/untouched.md"),
        "a concept with no origin or evidence in the week must not appear: {paths:?}"
    );
}

/// Ties on pages produced break toward the earlier session start.
#[test]
fn weekly_changes_top_session_tie_break_prefers_the_earlier_start() {
    let base = micros("2026-03-02T09:00:00Z");
    let sid_later = SessionId::new();
    let sid_earlier = SessionId::new();

    let overview = ProjectOverview {
        pages: vec![],
        sessions: vec![
            (sid_later, base + 3_600_000_000, "codex".to_string(), 5),
            (sid_earlier, base, "claude-code".to_string(), 5),
        ],
        first_session_us: Some(base),
        last_session_us: Some(base + 3_600_000_000),
    };

    let weeks = weekly_changes(&overview, 4);
    assert_eq!(weeks.len(), 1);
    let top = weeks[0]
        .top_session
        .clone()
        .expect("a week with sessions has a top session");
    assert_eq!(
        top.0, sid_earlier,
        "a tie on pages produced breaks toward the earlier start"
    );
}

/// A further tie on start time (and pages produced) falls back to the
/// lowest session id, deterministically.
#[test]
fn weekly_changes_top_session_tie_break_falls_back_to_the_lowest_id() {
    let base = micros("2026-03-02T09:00:00Z");
    let sid_a = SessionId::new();
    let sid_b = SessionId::new();
    let (lower, higher) = if sid_a.as_bytes() < sid_b.as_bytes() {
        (sid_a, sid_b)
    } else {
        (sid_b, sid_a)
    };

    let overview = ProjectOverview {
        pages: vec![],
        sessions: vec![
            (higher, base, "codex".to_string(), 3),
            (lower, base, "claude-code".to_string(), 3),
        ],
        first_session_us: Some(base),
        last_session_us: Some(base),
    };

    let weeks = weekly_changes(&overview, 4);
    let top = weeks[0].top_session.clone().unwrap();
    assert_eq!(
        top.0, lower,
        "an exact tie on produced and start breaks toward the lower session id"
    );
}

// --- `origin_counts_by_day` (pure) ---

/// Each page counts exactly once, on its origin day; same-day pages of
/// different kinds each get their own bucket, and a page with no origin is
/// not counted anywhere.
#[test]
fn origin_counts_by_day_counts_each_page_once_on_its_origin_day() {
    let day1 = micros("2026-09-17T08:00:00Z");
    let day1_later = micros("2026-09-17T20:00:00Z");
    let day2 = micros("2026-09-18T08:00:00Z");

    let overview = ProjectOverview {
        pages: vec![
            overview_page("decisions/a.md", "decision", Some(day1), vec![day1]),
            overview_page("gotchas/b.md", "gotcha", Some(day1_later), vec![day1_later]),
            overview_page("gotchas/c.md", "gotcha", Some(day2), vec![day2]),
            overview_page("concepts/d.md", "concept", None, vec![]),
        ],
        sessions: vec![],
        first_session_us: None,
        last_session_us: None,
    };

    let counts = origin_counts_by_day(&overview);
    assert_eq!(counts["2026-09-17"]["decision"], 1);
    assert_eq!(counts["2026-09-17"]["gotcha"], 1);
    assert_eq!(counts["2026-09-18"]["gotcha"], 1);
    assert_eq!(counts.get("2026-09-18").unwrap().get("decision"), None);

    let total: u32 = counts.values().flat_map(|m| m.values()).sum();
    assert_eq!(total, 3, "a page with no origin is never counted");
}

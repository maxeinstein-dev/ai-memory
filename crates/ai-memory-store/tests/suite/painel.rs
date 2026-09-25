//! `ReaderPool::timeline` (fork, painel web): sessions of a project and the
//! current pages each one produced (`page_evidence`, `source_kind =
//! 'session'`). Read-only; every query is filtered by (workspace_id,
//! project_id) per the fork's inherited security requirements
//! (docs/alfama/specs/2026-09-24-painel-web-alfama-design.md §2.1).

use std::collections::HashMap;

use ai_memory_core::{
    AgentKind, NewPage, NewSession, PageEvidence, PageEvidenceKind, PagePath, ProjectId, SessionId,
    Tier, WorkspaceId,
};
use ai_memory_store::{RuleCandidate, Store, f32_vec_to_bytes, group_rules};

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

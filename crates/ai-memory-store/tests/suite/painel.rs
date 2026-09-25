//! `ReaderPool::linha_do_tempo` (fork, painel web): sessions of a project and
//! the current pages each one produced (`page_evidence`, `source_kind =
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

async fn ended_session(store: &Store, ws: WorkspaceId, proj: ProjectId) -> SessionId {
    let session_id = SessionId::new();
    store
        .writer
        .begin_session(NewSession {
            id: session_id,
            workspace_id: ws,
            project_id: proj,
            agent_kind: AgentKind::ClaudeCode,
            cwd: None,
            actor_user: None,
        })
        .await
        .unwrap();
    store.writer.end_session(session_id, None).await.unwrap();
    session_id
}

/// The happy path: one ended session, one page it produced (cited via
/// `page_evidence`), shows up with the page attached.
#[tokio::test]
async fn linha_do_tempo_lista_sessoes_e_o_que_cada_uma_produziu() {
    let (_tmp, store, ws, proj) = seeded().await;
    let sid = ended_session(&store, ws, proj).await;

    let mut p = page(ws, proj, "gotchas/x.md", "Gotcha X", "corpo");
    p.evidence = vec![PageEvidence {
        kind: PageEvidenceKind::Session,
        source_id: sid.to_string(),
    }];
    store.writer.upsert_page(p).await.unwrap();

    let linha = store.reader.linha_do_tempo(ws, proj, 0).await.unwrap();
    assert_eq!(linha.len(), 1);
    assert_eq!(linha[0].id, sid.to_string());
    assert_eq!(linha[0].produziu.len(), 1);
    assert_eq!(linha[0].produziu[0].path, "gotchas/x.md");
    assert_eq!(linha[0].produziu[0].kind, "gotcha");
}

/// `desde_us` is an inclusive lower bound on `started_at`: a session that
/// started before the window must not appear, even though it produced a page.
/// The session here starts at real "now"; the window is pushed into the
/// future so it falls outside on the near side, mirroring how
/// `session_counts_by_agent`'s own `since` test proves exclusion (a future
/// cutoff, not a backdated row — the reader pool is read-only).
#[tokio::test]
async fn sessao_fora_da_janela_nao_aparece() {
    let (_tmp, store, ws, proj) = seeded().await;
    let sid = ended_session(&store, ws, proj).await;

    let mut p = page(ws, proj, "gotchas/x.md", "Gotcha X", "corpo");
    p.evidence = vec![PageEvidence {
        kind: PageEvidenceKind::Session,
        source_id: sid.to_string(),
    }];
    store.writer.upsert_page(p).await.unwrap();

    let desde_us = jiff::Timestamp::now().as_microsecond() + 60_000_000;
    let linha = store
        .reader
        .linha_do_tempo(ws, proj, desde_us)
        .await
        .unwrap();
    assert!(
        linha.is_empty(),
        "uma janela que comeca no futuro exclui a sessao"
    );
}

/// A page superseded by a newer version of itself (no longer `is_latest`)
/// must not count as something the citing session "produced" today.
#[tokio::test]
async fn pagina_nao_latest_nao_entra_em_produziu() {
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
    let p2 = page(
        ws,
        proj,
        "gotchas/x.md",
        "Gotcha X",
        "v2, sem nova evidencia",
    );
    store.writer.upsert_page(p2).await.unwrap();

    let linha = store.reader.linha_do_tempo(ws, proj, 0).await.unwrap();
    assert_eq!(linha.len(), 1);
    assert!(
        linha[0].produziu.is_empty(),
        "a versao superada nao deve aparecer como produzida"
    );
}

/// A page from another project must never leak into this project's timeline,
/// even if (hypothetically) it cited the same session id.
#[tokio::test]
async fn pagina_de_outro_projeto_nao_entra_em_produziu() {
    let (_tmp, store, ws, proj) = seeded().await;
    let outro_proj = store
        .writer
        .get_or_create_project(ws, "outro".to_string(), None)
        .await
        .unwrap();
    let sid = ended_session(&store, ws, proj).await;

    let mut p = page(ws, outro_proj, "gotchas/y.md", "Gotcha Y", "corpo");
    p.evidence = vec![PageEvidence {
        kind: PageEvidenceKind::Session,
        source_id: sid.to_string(),
    }];
    store.writer.upsert_page(p).await.unwrap();

    let linha = store.reader.linha_do_tempo(ws, proj, 0).await.unwrap();
    assert_eq!(linha.len(), 1);
    assert!(
        linha[0].produziu.is_empty(),
        "pagina de outro projeto nao deve aparecer"
    );
}

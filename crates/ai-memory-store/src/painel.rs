//! Read-only queries for the panel screens (fork). Nothing here writes.
//!
//! Every query is filtered by `(workspace_id, project_id)`, per the fork's
//! inherited security requirements
//! (docs/alfama/specs/2026-09-24-painel-web-alfama-design.md §2.1).

use ai_memory_core::{ProjectId, SessionId, WorkspaceId};
use rusqlite::params;

use crate::StoreResult;
use crate::reader::{ReaderPool, page_kind_expr};

/// One current page a session produced or reaffirmed (`page_evidence`,
/// `source_kind = 'session'`).
#[derive(Debug, Clone)]
pub struct PaginaProduzida {
    /// Path of the page's current (`is_latest`) version.
    pub path: String,
    /// Title of the page's current version.
    pub title: String,
    /// The page's kind (`rule`, `decision`, `concept`, `gotcha`, …), from
    /// [`page_kind_expr`].
    pub kind: String,
}

/// One session on a project's timeline, plus the current pages it produced.
#[derive(Debug, Clone)]
pub struct SessaoNaLinha {
    /// Session id, as its canonical string form (`SessionId::to_string`).
    pub id: String,
    /// Which agent CLI ran this session (`sessions.agent_kind`).
    pub agent: String,
    /// When the session started, in microseconds since the Unix epoch.
    pub started_us: i64,
    /// When the session ended, or `None` if it is still open.
    pub ended_us: Option<i64>,
    /// Observation count recorded at session end (0 while still open).
    pub observacoes: i64,
    /// Current pages this session produced or reaffirmed.
    pub produziu: Vec<PaginaProduzida>,
}

/// Raw `sessions` row before the id BLOB is resolved to a typed
/// [`SessionId`] — kept as a named struct rather than a tuple to avoid
/// clippy's `type_complexity` lint on the `query_map` collect.
struct RawSessionRow {
    id_bytes: Vec<u8>,
    agent: String,
    started_us: i64,
    ended_us: Option<i64>,
    observacoes: i64,
}

impl ReaderPool {
    /// Sessions of the project since `desde_us` (most recent first, capped at
    /// 500), each with the current (`is_latest`) pages it produced.
    pub async fn linha_do_tempo(
        &self,
        ws: WorkspaceId,
        proj: ProjectId,
        desde_us: i64,
    ) -> StoreResult<Vec<SessaoNaLinha>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, agent_kind, started_at, ended_at, ended_observation_count \
                 FROM sessions \
                 WHERE workspace_id = ?1 AND project_id = ?2 AND started_at >= ?3 \
                 ORDER BY started_at DESC LIMIT 500",
            )?;
            let raw_rows: Vec<RawSessionRow> = stmt
                .query_map(params![ws.as_bytes(), proj.as_bytes(), desde_us], |r| {
                    Ok(RawSessionRow {
                        id_bytes: r.get(0)?,
                        agent: r.get(1)?,
                        started_us: r.get(2)?,
                        ended_us: r.get(3)?,
                        observacoes: r.get(4)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            // Resolve the id BLOB through `SessionId::from_slice` — the same
            // helper `reader.rs` uses for every other session id — instead
            // of reaching for `uuid::Uuid::from_slice` directly.
            let mut out = Vec::with_capacity(raw_rows.len());
            for row in raw_rows {
                let id = SessionId::from_slice(&row.id_bytes)?.to_string();
                out.push(SessaoNaLinha {
                    id,
                    agent: row.agent,
                    started_us: row.started_us,
                    ended_us: row.ended_us,
                    observacoes: row.observacoes,
                    produziu: Vec::new(),
                });
            }

            let kind = page_kind_expr("pg.path", "pg.frontmatter_json");
            let mut ev = conn.prepare(&format!(
                "SELECT pe.source_id, pg.path, pg.title, {kind} FROM page_evidence pe \
                 JOIN pages pg ON pg.id = pe.page_id \
                 WHERE pe.source_kind = 'session' AND pg.workspace_id = ?1 AND pg.project_id = ?2 \
                 AND pg.is_latest = 1 ORDER BY pg.path"
            ))?;
            let rows = ev.query_map(params![ws.as_bytes(), proj.as_bytes()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    PaginaProduzida {
                        path: r.get(1)?,
                        title: r.get(2)?,
                        kind: r.get(3)?,
                    },
                ))
            })?;
            for row in rows {
                let (sid, pagina) = row?;
                if let Some(s) = out.iter_mut().find(|s| s.id == sid) {
                    s.produziu.push(pagina);
                }
            }
            Ok(out)
        })
        .await
    }
}

//! Read-only queries for the panel screens (fork). Nothing here writes.
//!
//! Every query is filtered by `(workspace_id, project_id)`, per the fork's
//! inherited security requirements
//! (docs/alfama/specs/2026-09-24-painel-web-alfama-design.md §2.1).

use std::collections::HashMap;

use ai_memory_core::{ProjectId, SessionId, WorkspaceId};
use rusqlite::{ToSql, params};

use crate::StoreResult;
use crate::reader::{ReaderPool, page_kind_expr};

/// One current page a session produced or reaffirmed (`page_evidence`,
/// `source_kind = 'session'`).
#[derive(Debug, Clone)]
pub struct ProducedPage {
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
pub struct TimelineSession {
    /// Session id, as its canonical string form (`SessionId::to_string`).
    pub id: String,
    /// Which agent CLI ran this session (`sessions.agent_kind`).
    pub agent: String,
    /// When the session started, in microseconds since the Unix epoch.
    pub started_us: i64,
    /// When the session ended, or `None` if it is still open.
    pub ended_us: Option<i64>,
    /// Observation count recorded at session end, or `None` while the
    /// session is still open. The persisted `ended_observation_count`
    /// column is `0` (not meaningful) until the session ends, so an open
    /// session must not report it as a real count of zero observations.
    pub observations: Option<i64>,
    /// Current pages this session produced or reaffirmed.
    pub produced: Vec<ProducedPage>,
}

/// Raw `sessions` row before the id BLOB is resolved to a typed
/// [`SessionId`] — kept as a named struct rather than a tuple to avoid
/// clippy's `type_complexity` lint on the `query_map` collect.
struct RawSessionRow {
    id_bytes: Vec<u8>,
    agent: String,
    started_us: i64,
    ended_us: Option<i64>,
    ended_observation_count: i64,
}

impl ReaderPool {
    /// Sessions of the project since `since_us` (most recent first, ties
    /// broken by id for deterministic ordering, capped at 500), each with
    /// the current (`is_latest`) pages it produced.
    pub async fn timeline(
        &self,
        ws: WorkspaceId,
        proj: ProjectId,
        since_us: i64,
    ) -> StoreResult<Vec<TimelineSession>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, agent_kind, started_at, ended_at, ended_observation_count \
                 FROM sessions \
                 WHERE workspace_id = ?1 AND project_id = ?2 AND started_at >= ?3 \
                 ORDER BY started_at DESC, id DESC LIMIT 500",
            )?;
            let raw_rows: Vec<RawSessionRow> = stmt
                .query_map(params![ws.as_bytes(), proj.as_bytes(), since_us], |r| {
                    Ok(RawSessionRow {
                        id_bytes: r.get(0)?,
                        agent: r.get(1)?,
                        started_us: r.get(2)?,
                        ended_us: r.get(3)?,
                        ended_observation_count: r.get(4)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            // Resolve the id BLOB through `SessionId::from_slice` — the same
            // helper `reader.rs` uses for every other session id — instead
            // of reaching for `uuid::Uuid::from_slice` directly.
            let mut out = Vec::with_capacity(raw_rows.len());
            for row in raw_rows {
                let id = SessionId::from_slice(&row.id_bytes)?.to_string();
                out.push(TimelineSession {
                    id,
                    agent: row.agent,
                    started_us: row.started_us,
                    // Only a session that has actually ended has a
                    // meaningful watermark; an open session's column is a
                    // `NOT NULL DEFAULT 0` placeholder, not a real count.
                    observations: row.ended_us.map(|_| row.ended_observation_count),
                    ended_us: row.ended_us,
                    produced: Vec::new(),
                });
            }

            if out.is_empty() {
                return Ok(out);
            }

            // Look up evidence only for the sessions we already selected
            // above (filtered by workspace/project/window), instead of
            // scanning every evidence row for the project and matching in a
            // nested loop. `page_evidence.source_id` is TEXT holding
            // `SessionId::to_string()` (hyphenated UUID), so the ids are
            // bound as strings here, not as the session BLOBs used above.
            let placeholders = std::iter::repeat_n("?", out.len())
                .collect::<Vec<_>>()
                .join(",");
            let kind = page_kind_expr("pg.path", "pg.frontmatter_json");
            let sql = format!(
                "SELECT pe.source_id, pg.path, pg.title, {kind} FROM page_evidence pe \
                 JOIN pages pg ON pg.id = pe.page_id \
                 WHERE pe.source_kind = 'session' AND pg.workspace_id = ? AND pg.project_id = ? \
                 AND pg.is_latest = 1 AND pe.source_id IN ({placeholders}) ORDER BY pg.path"
            );
            let mut ev = conn.prepare(&sql)?;
            let mut bound: Vec<&dyn ToSql> = Vec::with_capacity(out.len() + 2);
            let ws_bytes = ws.as_bytes();
            let proj_bytes = proj.as_bytes();
            bound.push(ws_bytes);
            bound.push(proj_bytes);
            for s in &out {
                bound.push(&s.id);
            }
            let rows = ev.query_map(bound.as_slice(), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    ProducedPage {
                        path: r.get(1)?,
                        title: r.get(2)?,
                        kind: r.get(3)?,
                    },
                ))
            })?;
            let mut by_session: HashMap<String, Vec<ProducedPage>> = HashMap::new();
            for row in rows {
                let (sid, produced_page) = row?;
                by_session.entry(sid).or_default().push(produced_page);
            }
            for s in &mut out {
                if let Some(pages) = by_session.remove(&s.id) {
                    s.produced = pages;
                }
            }
            Ok(out)
        })
        .await
    }
}

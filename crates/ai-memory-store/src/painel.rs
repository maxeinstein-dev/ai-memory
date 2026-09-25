//! Read-only queries for the panel screens (fork). Nothing here writes.
//!
//! Every query is filtered by `(workspace_id, project_id)`, per the fork's
//! inherited security requirements
//! (docs/alfama/specs/2026-09-24-painel-web-alfama-design.md §2.1).

use std::collections::{BTreeMap, HashMap};

use ai_memory_core::{PageId, ProjectId, SessionId, WorkspaceId};
use jiff::Timestamp;
use jiff::civil::Weekday;
use jiff::tz::TimeZone;
use rusqlite::{ToSql, params};
use uuid::Uuid;

use crate::StoreResult;
use crate::reader::{ReaderPool, page_kind_expr};

/// Machinery rather than knowledge: hidden from the project's "Recent
/// activity", the project overview, and collapsed into the sidebar's System
/// section. Underscore-prefixed trees are system surfaces — except
/// `_rules`, which holds standing human-authored rules — as are session
/// captures and the root-level bookkeeping pages (monthly logs, the OKF
/// bundle index, `_meta.md`).
///
/// Shared by `ai-memory-web` (the page tree) and
/// [`ReaderPool::project_overview`] (the overview screen), so the
/// knowledge/machinery split never drifts between the two — moved here from
/// `ai-memory-web/src/routes/project.rs`, which was its only prior home.
#[must_use]
pub fn is_system_page(path: &str) -> bool {
    if path.starts_with("_rules/") {
        return false;
    }
    if path.starts_with('_') || path.starts_with("sessions/") {
        return true;
    }
    if path.contains('/') {
        return false;
    }
    path == "index.md" || path == "_meta.md" || (path.starts_with("log-") && path.ends_with(".md"))
}

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

/// One current (`is_latest = 1`), non-system page of a project overview
/// (`ReaderPool::project_overview`), with its origin date and one-line
/// summary.
#[derive(Debug, Clone)]
pub struct OverviewPage {
    /// Path of the page's current version.
    pub path: String,
    /// Title of the page's current version.
    pub title: String,
    /// The page's kind (`rule`, `decision`, `concept`, `gotcha`, …), from
    /// [`page_kind_expr`].
    pub kind: String,
    /// One-line summary, from [`summary_line`].
    pub summary: Option<String>,
    /// Origin date: `MIN` of [`Self::evidence_starts_us`], or `None` when
    /// the page has no session evidence at all (`docs/alfama/specs/2026-09-25-visao-geral-design.md`
    /// §2.1). Never derived from `created_at`/`updated_at` — those are the
    /// date the LLM generated the page, not the date of the knowledge.
    pub origin_us: Option<i64>,
    /// Start times (microseconds since the epoch) of every *this scope's*
    /// session cited as evidence (`page_evidence`, `source_kind =
    /// 'session'`) for the page's current version, sorted ascending. A
    /// session belonging to another workspace or project never enters this
    /// list even when its id is cited (fact 4 of the plan).
    pub evidence_starts_us: Vec<i64>,
}

/// A project's overview: every current, non-system page with its origin
/// date, and every session in the scope with how many of those pages it
/// produced. Built by [`ReaderPool::project_overview`]; `summary_line`,
/// `iso_week_key`, `weekly_changes` and `origin_counts_by_day` derive the
/// overview screen's sections from it without touching the database again.
#[derive(Debug, Clone)]
pub struct ProjectOverview {
    /// The scope's current, non-system pages, ordered by path.
    pub pages: Vec<OverviewPage>,
    /// Every session of the scope: id, start time (microseconds since the
    /// epoch), agent, and how many of [`Self::pages`] it produced or
    /// reaffirmed (`page_evidence`).
    pub sessions: Vec<(SessionId, i64, String, u32)>,
    /// Start time of the scope's earliest session, or `None` when the scope
    /// has no sessions at all.
    pub first_session_us: Option<i64>,
    /// Start time of the scope's most recent session, or `None` when the
    /// scope has no sessions at all.
    pub last_session_us: Option<i64>,
}

/// Raw `pages` row for [`ReaderPool::project_overview`], before
/// `frontmatter_json` is parsed and the page's evidence is resolved — kept
/// as a named struct rather than a tuple, same reasoning as
/// [`RawSessionRow`].
struct RawOverviewPageRow {
    id: Vec<u8>,
    path: String,
    title: String,
    kind: String,
    frontmatter_json: String,
    body: String,
}

/// Raw `sessions` row for [`ReaderPool::project_overview`], before the id
/// BLOB is resolved to a typed [`SessionId`].
struct RawOverviewSessionRow {
    id: Vec<u8>,
    agent: String,
    started_us: i64,
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

    /// Latest `rule`-kind pages (via [`page_kind_expr`], or a `_rules/`
    /// path) across every project of one workspace, each carrying its
    /// current embedding only when the embedding's `(provider, model,
    /// dim)` is the most common triple in that workspace (invariant 8:
    /// a vector of a different triple is ignored, never compared).
    ///
    /// Scoped to `ws` — cross-*project* aggregation is the point of the
    /// "Entre projetos" screen, but it never crosses workspaces.
    ///
    /// # Errors
    /// Propagates any SQL or pool error.
    pub async fn rules_across_projects(&self, ws: WorkspaceId) -> StoreResult<Vec<RuleCandidate>> {
        self.with_conn(move |conn| {
            // The majority `(provider, model, dim)` triple for this
            // workspace's embeddings. `None` when the workspace has no
            // embeddings at all — every candidate then falls back to its
            // normalized title.
            let majority: Option<(String, String, u32)> = {
                let mut stmt = conn.prepare(
                    "SELECT pe.provider, pe.model, pe.dim, COUNT(*) AS c \
                     FROM page_embeddings pe \
                     JOIN pages pg ON pg.id = pe.page_id \
                     WHERE pg.workspace_id = ?1 \
                     GROUP BY pe.provider, pe.model, pe.dim \
                     ORDER BY c DESC, pe.provider ASC, pe.model ASC, pe.dim ASC \
                     LIMIT 1",
                )?;
                let mut rows = stmt.query(params![ws.as_bytes()])?;
                match rows.next()? {
                    Some(row) => {
                        let provider: String = row.get(0)?;
                        let model: String = row.get(1)?;
                        let dim: i64 = row.get(2)?;
                        Some((provider, model, u32::try_from(dim.max(0)).unwrap_or(0)))
                    }
                    None => None,
                }
            };

            let kind = page_kind_expr("pg.path", "pg.frontmatter_json");
            let sql = format!(
                "SELECT pr.name, pg.path, pg.title, pe.vector, pe.provider, pe.model, pe.dim \
                 FROM pages pg \
                 JOIN projects pr ON pr.id = pg.project_id \
                 LEFT JOIN page_embeddings pe ON pe.page_id = pg.id \
                 WHERE pg.workspace_id = ?1 AND pg.is_latest = 1 \
                   AND ({kind} = 'rule' OR pg.path LIKE '\\_rules/%' ESCAPE '\\') \
                 ORDER BY pr.name, pg.path"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![ws.as_bytes()], |r| {
                let dim: Option<i64> = r.get(6)?;
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<Vec<u8>>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    dim,
                ))
            })?;

            let mut out = Vec::new();
            for row in rows {
                let (project, path, title, vec_bytes, provider, model, dim) = row?;
                let dim = dim.map(|d| u32::try_from(d.max(0)).unwrap_or(0));
                let vector = match (&majority, vec_bytes, provider, model, dim) {
                    (
                        Some((maj_provider, maj_model, maj_dim)),
                        Some(bytes),
                        Some(p),
                        Some(m),
                        Some(d),
                    ) if p == *maj_provider && m == *maj_model && d == *maj_dim => {
                        decode_vector(&bytes, d)
                    }
                    _ => None,
                };
                out.push(RuleCandidate {
                    workspace: ws,
                    project,
                    path,
                    title,
                    vector,
                });
            }
            Ok(out)
        })
        .await
    }

    /// Assemble a project's overview: every current (`is_latest = 1`)
    /// non-system page with its origin date and one-line summary, and every
    /// session of the scope with how many of those pages it produced.
    ///
    /// The origin date of a page is `MIN(sessions.started_at)` over its
    /// `page_evidence` rows with `source_kind = 'session'`, resolved
    /// through a map of *this scope's* sessions only — a `source_id` that
    /// parses but names a session of another workspace or project simply
    /// misses the map and does not count (fact 4,
    /// `docs/alfama/plans/2026-09-25-visao-geral.md`), the same care as
    /// [`Self::timeline`]. A page with no such evidence has no origin date
    /// and is never dated by `created_at`/`updated_at` — those are the date
    /// the LLM generated the page, not the date of the knowledge (§1.3 of
    /// `docs/alfama/specs/2026-09-25-visao-geral-design.md`).
    ///
    /// # Errors
    /// Propagates any SQL or pool error.
    pub async fn project_overview(
        &self,
        ws: WorkspaceId,
        proj: ProjectId,
    ) -> StoreResult<ProjectOverview> {
        self.with_conn(move |conn| {
            let kind = page_kind_expr("path", "frontmatter_json");
            let mut page_stmt = conn.prepare(&format!(
                "SELECT id, path, title, {kind} AS kind, frontmatter_json, body \
                 FROM pages \
                 WHERE workspace_id = ?1 AND project_id = ?2 AND is_latest = 1 \
                 ORDER BY path"
            ))?;
            let raw_pages: Vec<RawOverviewPageRow> = page_stmt
                .query_map(params![ws.as_bytes(), proj.as_bytes()], |r| {
                    Ok(RawOverviewPageRow {
                        id: r.get(0)?,
                        path: r.get(1)?,
                        title: r.get(2)?,
                        kind: r.get(3)?,
                        frontmatter_json: r.get(4)?,
                        body: r.get(5)?,
                    })
                })?
                .collect::<Result<_, _>>()?;

            let mut sess_stmt = conn.prepare(
                "SELECT id, agent_kind, started_at FROM sessions \
                 WHERE workspace_id = ?1 AND project_id = ?2 \
                 ORDER BY started_at DESC, id DESC",
            )?;
            let raw_sessions: Vec<RawOverviewSessionRow> = sess_stmt
                .query_map(params![ws.as_bytes(), proj.as_bytes()], |r| {
                    Ok(RawOverviewSessionRow {
                        id: r.get(0)?,
                        agent: r.get(1)?,
                        started_us: r.get(2)?,
                    })
                })?
                .collect::<Result<_, _>>()?;

            // Map of *this scope's* sessions only, keyed by the session's
            // raw uuid. Evidence resolves through this map, never a bare
            // `Uuid::parse`, so a `source_id` naming a real session of
            // another project/workspace simply misses and does not count.
            let mut session_by_uuid: HashMap<Uuid, i64> =
                HashMap::with_capacity(raw_sessions.len());
            let mut sessions: Vec<(SessionId, i64, String, u32)> =
                Vec::with_capacity(raw_sessions.len());
            for row in raw_sessions {
                let id = SessionId::from_slice(&row.id)?;
                session_by_uuid.insert(id.0, row.started_us);
                sessions.push((id, row.started_us, row.agent, 0));
            }

            // Evidence, scoped by the *citing page's* (workspace, project)
            // through the join (the page side of §2.1's requirement); the
            // *session* side is checked separately through
            // `session_by_uuid`, which holds only this same scope's
            // sessions.
            let mut ev_stmt = conn.prepare(
                "SELECT pe.page_id, pe.source_id FROM page_evidence pe \
                 JOIN pages pg ON pg.id = pe.page_id \
                 WHERE pe.source_kind = 'session' AND pg.workspace_id = ?1 \
                   AND pg.project_id = ?2 AND pg.is_latest = 1",
            )?;
            let ev_rows: Vec<(Vec<u8>, String)> = ev_stmt
                .query_map(params![ws.as_bytes(), proj.as_bytes()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<Result<_, _>>()?;

            let mut starts_by_page: HashMap<PageId, Vec<i64>> = HashMap::new();
            let mut produced_by_session: HashMap<Uuid, u32> = HashMap::new();
            for (page_id_bytes, source_id) in ev_rows {
                let page_id = PageId::from_slice(&page_id_bytes)?;
                let Ok(uuid) = source_id.parse::<Uuid>() else {
                    continue;
                };
                let Some(&started_us) = session_by_uuid.get(&uuid) else {
                    continue;
                };
                starts_by_page.entry(page_id).or_default().push(started_us);
                *produced_by_session.entry(uuid).or_insert(0) += 1;
            }

            for s in &mut sessions {
                if let Some(&count) = produced_by_session.get(&s.0.0) {
                    s.3 = count;
                }
            }

            let mut pages = Vec::with_capacity(raw_pages.len());
            for row in raw_pages {
                if is_system_page(&row.path) {
                    continue;
                }
                let page_id = PageId::from_slice(&row.id)?;
                let mut evidence_starts_us = starts_by_page.remove(&page_id).unwrap_or_default();
                evidence_starts_us.sort_unstable();
                let origin_us = evidence_starts_us.first().copied();
                let frontmatter: serde_json::Value =
                    serde_json::from_str(&row.frontmatter_json).unwrap_or(serde_json::Value::Null);
                let summary = summary_line(&frontmatter, &row.body);
                pages.push(OverviewPage {
                    path: row.path,
                    title: row.title,
                    kind: row.kind,
                    summary,
                    origin_us,
                    evidence_starts_us,
                });
            }

            let first_session_us = sessions.iter().map(|s| s.1).min();
            let last_session_us = sessions.iter().map(|s| s.1).max();

            Ok(ProjectOverview {
                pages,
                sessions,
                first_session_us,
                last_session_us,
            })
        })
        .await
    }
}

/// One rule-like page (`kind = rule`, or `_rules/…`) as a candidate for
/// cross-project grouping (`docs/alfama/specs/2026-09-24-painel-web-alfama-design.md`
/// §3.4).
#[derive(Debug, Clone)]
pub struct RuleCandidate {
    /// Workspace the candidate belongs to. Grouping never crosses
    /// workspaces; this is carried along for the caller's bookkeeping.
    pub workspace: WorkspaceId,
    /// Owning project's name.
    pub project: String,
    /// Path of the page's current (`is_latest`) version.
    pub path: String,
    /// Title of the page's current version.
    pub title: String,
    /// The page's current embedding, decoded to `f32`s — only when its
    /// `(provider, model, dim)` matched the workspace's majority triple
    /// and the stored bytes decoded cleanly. `None` otherwise (invariant
    /// 8, or a malformed/absent embedding), in which case grouping falls
    /// back to the normalized title.
    pub vector: Option<Vec<f32>>,
}

/// A set of `RuleCandidate`s from at least two distinct projects judged to
/// say the same thing, per [`group_rules`].
#[derive(Debug, Clone)]
pub struct RuleGroup {
    /// Member rules, sorted by `(project, path)` for deterministic output.
    pub rules: Vec<RuleCandidate>,
}

impl RuleGroup {
    /// Distinct project names among this group's members, sorted.
    #[must_use]
    pub fn projects(&self) -> Vec<String> {
        let mut projects: Vec<String> = self.rules.iter().map(|r| r.project.clone()).collect();
        projects.sort();
        projects.dedup();
        projects
    }
}

/// Decode a little-endian `f32` vector packed by `f32_vec_to_bytes`
/// (`reader.rs`). Returns `None` — never panics — on a malformed blob: a
/// length that is not a multiple of 4, or one that does not match `dim`.
fn decode_vector(bytes: &[u8], dim: u32) -> Option<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    if bytes.len() != (dim as usize) * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(dim as usize);
    for chunk in bytes.chunks_exact(4) {
        // `chunks_exact(4)` guarantees exactly 4 bytes per chunk.
        let arr: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
        out.push(f32::from_le_bytes(arr));
    }
    Some(out)
}

/// Cosine similarity of two equal-length vectors. `None` when the lengths
/// differ (never compare vectors of different dims) or either vector has
/// zero norm (undefined direction, guarded rather than dividing by zero).
fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() {
        return None;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return None;
    }
    Some(dot / (norm_a * norm_b))
}

/// Lowercase a title and strip a small fixed table of accented Latin
/// letters common in Portuguese titles (no new crate for this — same
/// approach as the `NoLegacyDomainTest` normalization in the SGW project).
fn strip_accents(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// Normalize a title for the no-vector grouping fallback: lowercase,
/// accents stripped, whitespace collapsed.
fn normalize_title(title: &str) -> String {
    strip_accents(&title.to_lowercase())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Group rule candidates that likely say the same thing, keeping only
/// groups that span at least two distinct projects (a repeated rule
/// within a single project is not a candidate for promotion).
///
/// Two candidates are linked when either:
/// - both carry a vector of equal length and their cosine similarity is
///   at least `threshold` (never compares vectors of different dims); or
/// - neither carries a vector and their normalized titles are equal.
///
/// A union-find over these pairwise links forms the groups; output order
/// is deterministic (each group's members sorted by `(project, path)`,
/// groups themselves sorted by their first member).
#[must_use]
pub fn group_rules(rules: Vec<RuleCandidate>, threshold: f32) -> Vec<RuleGroup> {
    let n = rules.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], x: usize) -> usize {
        if parent[x] != x {
            parent[x] = find(parent, parent[x]);
        }
        parent[x]
    }

    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            parent[hi] = lo;
        }
    }

    let title_keys: Vec<Option<String>> = rules
        .iter()
        .map(|r| {
            if r.vector.is_none() {
                Some(normalize_title(&r.title))
            } else {
                None
            }
        })
        .collect();

    for i in 0..n {
        for j in (i + 1)..n {
            let linked = match (&rules[i].vector, &rules[j].vector) {
                (Some(a), Some(b)) => cosine(a, b).is_some_and(|c| c >= threshold),
                (None, None) => title_keys[i] == title_keys[j],
                _ => false,
            };
            if linked {
                union(&mut parent, i, j);
            }
        }
    }

    let mut by_root: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        by_root.entry(root).or_default().push(i);
    }

    let mut groups: Vec<RuleGroup> = Vec::new();
    for idxs in by_root.into_values() {
        let mut members: Vec<RuleCandidate> = idxs.into_iter().map(|i| rules[i].clone()).collect();
        let mut projects: Vec<&str> = members.iter().map(|r| r.project.as_str()).collect();
        projects.sort_unstable();
        projects.dedup();
        if projects.len() < 2 {
            continue;
        }
        members.sort_by(|a, b| {
            (a.project.as_str(), a.path.as_str()).cmp(&(b.project.as_str(), b.path.as_str()))
        });
        groups.push(RuleGroup { rules: members });
    }

    groups.sort_by(|a, b| {
        let ka = (a.rules[0].project.as_str(), a.rules[0].path.as_str());
        let kb = (b.rules[0].project.as_str(), b.rules[0].path.as_str());
        ka.cmp(&kb)
    });
    groups
}

// --- Project overview: pure functions (testable without a database) ---

/// Upper bound on a project-overview summary line, in `char`s (never bytes —
/// a cut must land on a `char` boundary).
const SUMMARY_MAX_CHARS: usize = 200;

/// One-line summary for a page in the project overview: the frontmatter
/// `summary` when the page has a non-blank one, otherwise the first real
/// paragraph of the body. Trimmed and cut to [`SUMMARY_MAX_CHARS`] on a
/// `char` boundary, with an ellipsis appended when the text was actually
/// cut. `None` only when neither source yields any text at all.
#[must_use]
pub fn summary_line(frontmatter_json: &serde_json::Value, body: &str) -> Option<String> {
    let from_frontmatter = frontmatter_json
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let candidate = match from_frontmatter {
        Some(s) => s.to_owned(),
        None => first_paragraph(body)?,
    };
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_chars_with_ellipsis(trimmed, SUMMARY_MAX_CHARS))
}

/// First non-empty paragraph of a page body, skipping a leading YAML
/// frontmatter fence — `NewPage::body` is documented to exclude it already,
/// but this is a standalone pure function and does not lean on that
/// invariant holding forever — and any heading/rule line (`#`,
/// `---`/`___`/`***`). A paragraph is a run of consecutive non-blank lines,
/// joined with a single space. `None` when the body has no prose at all.
fn first_paragraph(body: &str) -> Option<String> {
    let mut lines = body.lines().peekable();

    // Skip a leading frontmatter fence, if the body happens to carry one.
    if lines.peek().map(|l| l.trim()) == Some("---") {
        lines.next();
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
        }
    }

    let mut paragraph: Vec<&str> = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if is_heading_or_rule(trimmed) {
            continue;
        }
        paragraph.push(trimmed);
    }
    if paragraph.is_empty() {
        None
    } else {
        Some(paragraph.join(" "))
    }
}

/// `true` for a markdown heading (`#`) or horizontal-rule line
/// (`---`/`___`/`***`), which carry structure rather than prose.
fn is_heading_or_rule(line: &str) -> bool {
    line.starts_with('#')
        || line.starts_with("---")
        || line.starts_with("___")
        || line.starts_with("***")
}

/// Truncate to at most `max` `char`s, on a `char` boundary, appending an
/// ellipsis when the text was actually cut.
fn truncate_chars_with_ellipsis(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// UTC calendar date of a microsecond timestamp. Every caller derives `us`
/// from a stored timestamp (`sessions.started_at`, or an
/// `origin_us`/`evidence_starts_us` derived from it), always within jiff's
/// representable range in practice; the fallback (the Unix epoch, always
/// representable) exists only so a pure function never panics on a
/// pathological value outside that range.
fn utc_date(us: i64) -> jiff::civil::Date {
    let ts = Timestamp::from_microsecond(us).unwrap_or_else(|_| {
        Timestamp::from_microsecond(0).expect("microsecond 0 (the Unix epoch) is always in range")
    });
    ts.to_zoned(TimeZone::UTC).date()
}

/// UTC calendar day of a microsecond timestamp, as `YYYY-MM-DD`.
fn utc_day_key(us: i64) -> String {
    let d = utc_date(us);
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

/// ISO-8601 (year, week) of a microsecond timestamp, via `jiff`. The ISO
/// year can differ from the calendar year in the last days of December and
/// the first days of January — e.g. 2026-12-31 and 2027-01-01 are both ISO
/// week 53 of 2026, not week 1 of 2027 (the year of a week is the year of
/// that week's Thursday).
#[must_use]
pub fn iso_week_key(us: i64) -> (i32, u8) {
    let iso = utc_date(us).iso_week_date();
    (i32::from(iso.year()), u8::try_from(iso.week()).unwrap_or(1))
}

/// Days from `weekday` back to the Monday starting its week (`0` for
/// Monday, …, `6` for Sunday).
fn days_since_monday(weekday: Weekday) -> i64 {
    match weekday {
        Weekday::Monday => 0,
        Weekday::Tuesday => 1,
        Weekday::Wednesday => 2,
        Weekday::Thursday => 3,
        Weekday::Friday => 4,
        Weekday::Saturday => 5,
        Weekday::Sunday => 6,
    }
}

/// Microseconds in one day (UTC, no leap seconds — matching every other
/// timestamp in this codebase).
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// UTC date of the Monday starting the ISO week containing `any_us_in_week`,
/// as `YYYY-MM-DD`. Takes any timestamp already known to fall in the target
/// week (every caller has one to hand — a session that started in it),
/// which sidesteps reconstructing a date from a bare `(year, week)` pair.
fn iso_week_start_date(any_us_in_week: i64) -> String {
    let date = utc_date(any_us_in_week);
    let back_to_monday = days_since_monday(date.weekday());
    let monday = utc_date(any_us_in_week - back_to_monday * MICROS_PER_DAY);
    format!(
        "{:04}-{:02}-{:02}",
        monday.year(),
        monday.month(),
        monday.day()
    )
}

/// `true` when `page`'s origin date falls inside the ISO `(year, week)`.
fn page_is_new_in_week(page: &OverviewPage, year: i32, week: u8) -> bool {
    page.origin_us
        .is_some_and(|us| iso_week_key(us) == (year, week))
}

/// `true` when `page` already existed before the ISO `(year, week)` (its
/// origin is in an earlier week) but picked up at least one more evidence
/// session that started during it — a re-affirmation rather than a fresh
/// page.
fn page_is_updated_in_week(page: &OverviewPage, year: i32, week: u8) -> bool {
    match page.origin_us {
        Some(origin_us) if iso_week_key(origin_us) != (year, week) => page
            .evidence_starts_us
            .iter()
            .any(|&us| iso_week_key(us) == (year, week)),
        _ => false,
    }
}

/// One ISO week (Monday-Sunday) of project activity, for the "Últimas
/// grandes mudanças" section of the project overview (§2.2 item 2 of
/// `docs/alfama/specs/2026-09-25-visao-geral-design.md`).
#[derive(Debug, Clone)]
pub struct WeekChanges {
    /// ISO year (may differ from the calendar year in the last/first days
    /// of December/January — see [`iso_week_key`]).
    pub year: i32,
    /// ISO week number (1..=53).
    pub week: u8,
    /// UTC calendar date of the week's Monday, as `YYYY-MM-DD`.
    pub start_date: String,
    /// Number of sessions that started in this week.
    pub sessions: u32,
    /// Decision pages new this week (origin date inside the week), ordered
    /// by origin date (ties broken by path).
    pub new_decisions: Vec<OverviewPage>,
    /// Concept pages new this week, or with an earlier origin that picked
    /// up evidence this week ("updated"). Ordered by path.
    pub new_or_updated_concepts: Vec<OverviewPage>,
    /// The session that produced the most of this week's pages. Ties break
    /// on the earliest start, then on the lowest session id; `None` only
    /// when the week (impossibly, since it only exists because a session
    /// started in it) has no sessions.
    pub top_session: Option<(SessionId, i64, String, u32)>,
}

/// Weekly breakdown of a project's activity (§2.2 item 2 of the design
/// spec), most recent active week first, capped at `max_weeks`. A week
/// enters the list only when at least one session started in it
/// ("atividade") — a project can have far more silent weeks than active
/// ones. Built entirely from `overview`, without touching the database
/// again.
#[must_use]
pub fn weekly_changes(overview: &ProjectOverview, max_weeks: usize) -> Vec<WeekChanges> {
    // Group session *indices* (not references or clones) by ISO (year,
    // week): `Vec<usize>` keeps this map's type trivial, where a spelled-out
    // `Vec<&(SessionId, i64, String, u32)>` value would trip clippy's
    // `type_complexity` lint.
    let mut sessions_by_week: BTreeMap<(i32, u8), Vec<usize>> = BTreeMap::new();
    for (idx, s) in overview.sessions.iter().enumerate() {
        sessions_by_week
            .entry(iso_week_key(s.1))
            .or_default()
            .push(idx);
    }

    sessions_by_week
        .keys()
        .rev()
        .take(max_weeks)
        .map(|&(year, week)| {
            let sessions_in_week: Vec<_> = sessions_by_week[&(year, week)]
                .iter()
                .map(|&idx| &overview.sessions[idx])
                .collect();

            let mut new_decisions: Vec<OverviewPage> = overview
                .pages
                .iter()
                .filter(|p| p.kind == "decision" && page_is_new_in_week(p, year, week))
                .cloned()
                .collect();
            new_decisions.sort_by(|a, b| {
                (a.origin_us, a.path.as_str()).cmp(&(b.origin_us, b.path.as_str()))
            });

            let mut new_or_updated_concepts: Vec<OverviewPage> = overview
                .pages
                .iter()
                .filter(|p| {
                    p.kind == "concept"
                        && (page_is_new_in_week(p, year, week)
                            || page_is_updated_in_week(p, year, week))
                })
                .cloned()
                .collect();
            new_or_updated_concepts.sort_by(|a, b| a.path.cmp(&b.path));

            let mut candidates = sessions_in_week.clone();
            candidates.sort_by(|a, b| {
                b.3.cmp(&a.3)
                    .then_with(|| a.1.cmp(&b.1))
                    .then_with(|| a.0.as_bytes().cmp(b.0.as_bytes()))
            });
            let top_session = candidates
                .first()
                .map(|&&(id, started_us, ref agent, produced)| {
                    (id, started_us, agent.clone(), produced)
                });

            let start_date = sessions_in_week
                .first()
                .map(|s| iso_week_start_date(s.1))
                .unwrap_or_default();

            WeekChanges {
                year,
                week,
                start_date,
                sessions: u32::try_from(sessions_in_week.len()).unwrap_or(u32::MAX),
                new_decisions,
                new_or_updated_concepts,
                top_session,
            }
        })
        .collect()
}

/// Count of each non-system page kind, on the UTC calendar day of the
/// page's origin ([`OverviewPage::origin_us`]) — each page counted exactly
/// once, on the single day it originated, never on every day its evidence
/// touches. A page with no origin is not counted here; it surfaces in the
/// "sem data de origem" section instead. Feeds the timeline's per-day
/// breakdown (§2.3 of the design spec).
#[must_use]
pub fn origin_counts_by_day(overview: &ProjectOverview) -> BTreeMap<String, BTreeMap<String, u32>> {
    let mut out: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    for page in &overview.pages {
        let Some(origin_us) = page.origin_us else {
            continue;
        };
        let day = utc_day_key(origin_us);
        *out.entry(day)
            .or_default()
            .entry(page.kind.clone())
            .or_insert(0) += 1;
    }
    out
}

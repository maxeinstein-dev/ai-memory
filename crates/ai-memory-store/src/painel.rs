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

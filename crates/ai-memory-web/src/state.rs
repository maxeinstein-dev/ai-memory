//! Web router state — the handle a request handler receives.
//!
//! Holds the read-only store pool + the wiki handle, plus (since the
//! `POST /handoff` route — the panel's first write path, see
//! `docs/alfama/specs/2026-09-28-handoff-pela-tela.md`) the write handle and
//! sanitizer every other write-capable surface already carries. Cheap to
//! clone (everything inside is `Arc`-shaped already), so axum's
//! `State<Arc<WebState>>` extractor stays free of clone-heavy code.

use ai_memory_core::Sanitizer;
use ai_memory_store::{ReaderPool, WriterHandle};
use ai_memory_wiki::Wiki;

/// Shared state for every web route. Construct once via
/// [`crate::router`].
#[derive(Clone)]
pub struct WebState {
    /// Read-only SQLite pool — drives FTS5 search, page metadata,
    /// project list aggregates.
    pub reader: ReaderPool,
    /// Wiki handle — reads page bodies from disk.
    pub wiki: Wiki,
    /// Write handle. Unused by every route except `POST /handoff` — every
    /// other route in this crate stays read-only by construction.
    pub writer: WriterHandle,
    /// Privacy strip applied to handoff free-text before it is written,
    /// exactly like every other handoff-creating surface.
    pub sanitizer: Sanitizer,
}

impl WebState {
    /// Build a new shared state.
    ///
    /// `writer` and `sanitizer` are pulled off `wiki` rather than added as
    /// their own parameters: `serve.rs` already builds `wiki` from exactly
    /// those two values (`Wiki::new(&data_dir, store.writer.clone())
    /// .with_sanitizer(sanitizer.clone())`), and `Wiki` exposes both back
    /// out via `writer()`/`sanitizer()`. Threading them through here again
    /// as separate constructor parameters would mean also widening
    /// `crate::router`/`crate::api_router`, and updating every one of their
    /// ~90 test call sites, to pass values the `wiki` argument already
    /// carries.
    #[must_use]
    pub fn new(reader: ReaderPool, wiki: Wiki) -> Self {
        let writer = wiki.writer().clone();
        let sanitizer = wiki.sanitizer().clone();
        Self {
            reader,
            wiki,
            writer,
            sanitizer,
        }
    }
}

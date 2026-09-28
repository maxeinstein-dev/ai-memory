//! Web router state — the handle a request handler receives.
//!
//! Holds the read-only store pool + the wiki handle, plus (since the
//! `POST /handoff` route — the panel's first write path, see
//! `docs/alfama/specs/2026-09-28-handoff-pela-tela.md`) the write handle and
//! sanitizer every other write-capable surface already carries. Cheap to
//! clone (everything inside is `Arc`-shaped already), so axum's
//! `State<Arc<WebState>>` extractor stays free of clone-heavy code.

use std::sync::Arc;

use ai_memory_core::Sanitizer;
use ai_memory_store::{ReaderPool, WriterHandle};
use ai_memory_wiki::Wiki;

use crate::csrf::CsrfKey;

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
    /// Process-lifetime CSRF key for `POST /handoff` (see `crate::csrf`).
    /// `Arc`-wrapped so every clone of `WebState` shares the exact key
    /// generated once in [`Self::new`] — regenerating per clone would make
    /// a token issued by one clone unverifiable by another.
    pub csrf_key: Arc<CsrfKey>,
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
        // OS RNG failure here is not a recoverable condition for a process
        // that is about to serve a write route — same posture as the other
        // `getrandom::fill` call sites in this workspace (auth tokens, user
        // salts), which propagate the error up to startup rather than
        // limping on with a predictable key.
        let csrf_key = Arc::new(CsrfKey::generate().expect("OS RNG must be available at startup"));
        Self {
            reader,
            wiki,
            writer,
            sanitizer,
            csrf_key,
        }
    }
}

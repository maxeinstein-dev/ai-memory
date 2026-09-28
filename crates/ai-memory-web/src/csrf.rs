//! CSRF defense for `POST /handoff` — the panel's first write route.
//!
//! See `docs/alfama/specs/2026-09-28-handoff-pela-tela.md` §4.3. The server
//! has no session cookie, so there is no native origin defense; a page on
//! another origin can still make the browser *send* a `POST` to this local
//! server (CORS only blocks it from *reading* the response). The defense is
//! a stateless HMAC token embedded in the form when `page.html` renders:
//! a cross-origin page can trigger the request but can never read the GET
//! response that carries the token, so it can never mint a valid one.
//!
//! - [`CsrfKey`] is generated once per process, in memory only — it does not
//!   need to survive a restart, and restarting simply invalidates every
//!   in-flight form (same UX as a token that expired).
//! - [`issue`] embeds the token in the rendered form.
//! - [`verify`] recomputes it from the POST body's own scope fields and
//!   compares in constant time, tolerating the current and previous minute
//!   (~2 minutes) of clock skew between render and submit.
//!
//! HMAC-SHA256 is hand-rolled from `sha2` (already a workspace dependency)
//! instead of pulling in the `hmac` crate, which is not currently a
//! dependency anywhere in this workspace — RFC 2104 over a hash this crate
//! already links is a dozen lines, not worth a new dependency + `Cargo.lock`
//! churn for. Constant-time comparison uses `subtle`, which unlike `hmac`
//! *is* already a workspace dependency (`ai-memory-mcp`, `ai-memory-store`),
//! so reusing it here adds no new dependency at all.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const SHA256_BLOCK_LEN: usize = 64;

/// Current time floored to whole minutes, the unit [`issue`]/[`verify`] key
/// on. Shared so the render side (`page.html`'s handler) and the verify
/// side (`POST /handoff`) can never drift on how "now" is computed.
#[must_use]
pub fn now_unix_minute() -> i64 {
    jiff::Timestamp::now().as_second() / 60
}

/// Process-lifetime HMAC key for the CSRF token. Generated once at server
/// startup via [`CsrfKey::generate`] and held only in memory.
#[derive(Clone)]
pub struct CsrfKey([u8; 32]);

impl CsrfKey {
    /// Generate a fresh random key. Uses `getrandom` directly (already a
    /// workspace dependency, and the same primitive
    /// `ai-memory-store`/`ai-memory-mcp` already use for tokens/salts)
    /// rather than adding `rand` as a new direct dependency of this crate.
    ///
    /// # Errors
    /// Propagates the underlying OS RNG failure, which is not expected in
    /// practice (see the sibling call sites in `ai-memory-store`).
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf)?;
        Ok(Self(buf))
    }

    /// Build a key from raw bytes. Test-only: production always goes through
    /// [`Self::generate`]. Not `#[cfg(test)]`-gated because it is also used
    /// by `crate::router_with_csrf_key_for_test`, the doc-hidden test entry
    /// point the `tests/suite` integration binary calls to make CSRF tokens
    /// deterministic (same pattern as `montar_briefing_para_teste` in
    /// `lib.rs`).
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// RFC 2104 HMAC-SHA256. Manual implementation — see the module docs for why
/// this doesn't pull in the `hmac` crate. `key` is always exactly 32 bytes
/// here ([`CsrfKey`]), well under the 64-byte SHA-256 block size, so the
/// "hash the key down first" branch of the RFC is intentionally not
/// implemented — it would be dead code for every caller in this module.
fn hmac_sha256(key: &[u8; 32], msg: &[u8]) -> [u8; 32] {
    let mut ipad = [0x36u8; SHA256_BLOCK_LEN];
    let mut opad = [0x5cu8; SHA256_BLOCK_LEN];
    for (i, byte) in key.iter().enumerate() {
        ipad[i] ^= byte;
        opad[i] ^= byte;
    }
    let inner = {
        let mut hasher = Sha256::new();
        hasher.update(ipad);
        hasher.update(msg);
        hasher.finalize()
    };
    let outer = {
        let mut hasher = Sha256::new();
        hasher.update(opad);
        hasher.update(inner);
        hasher.finalize()
    };
    outer.into()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Build the byte string the token is computed over.
///
/// Each of `from_workspace`/`from_project`/`from_path` is length-prefixed
/// (an 8-byte big-endian length, then the field's own bytes) rather than
/// joined with a separator byte. A separator (this used `\0` originally)
/// does NOT prevent a framing collision when a field can itself contain that
/// separator: workspace/project names are arbitrary caller-supplied strings
/// with no byte excluded, so `("a\0b", "c")` and `("a", "b\0c")` produced the
/// exact same joined bytes (`a\0b\0c\0`) under the old scheme, meaning a
/// token issued for one scope would also verify for the other. Length-
/// prefixing removes the ambiguity unconditionally — the boundary between
/// fields is determined by a count, never by scanning for a byte value that
/// might also occur inside the field — with no new input validation surface
/// (no byte is rejected or escaped).
fn signing_message(
    from_workspace: &str,
    from_project: &str,
    from_path: &str,
    minute: i64,
) -> Vec<u8> {
    let mut msg =
        Vec::with_capacity(from_workspace.len() + from_project.len() + from_path.len() + 32);
    for field in [from_workspace, from_project, from_path] {
        msg.extend_from_slice(&(field.len() as u64).to_be_bytes());
        msg.extend_from_slice(field.as_bytes());
    }
    msg.extend_from_slice(&minute.to_be_bytes());
    msg
}

/// Issue a CSRF token for a page's own scope, embedded as a hidden form
/// field when `page.html` renders. `now_unix_minute` is the current time
/// floored to whole minutes (`unix_seconds / 60`).
#[must_use]
pub fn issue(
    key: &CsrfKey,
    from_workspace: &str,
    from_project: &str,
    from_path: &str,
    now_unix_minute: i64,
) -> String {
    let msg = signing_message(from_workspace, from_project, from_path, now_unix_minute);
    hex_encode(&hmac_sha256(&key.0, &msg))
}

/// Verify a submitted CSRF token against the `from_*` fields taken from the
/// **POST body itself** (never from a value the caller derived some other
/// way — the whole point is that the token is bound to exactly the scope it
/// was issued for). Accepts the current minute and the previous one (~2
/// minutes of tolerance for render-to-submit latency and clock rounding).
/// Comparison is constant-time so response timing cannot leak how many
/// leading bytes of a guessed token matched.
#[must_use]
pub fn verify(
    key: &CsrfKey,
    token: &str,
    from_workspace: &str,
    from_project: &str,
    from_path: &str,
    now_unix_minute: i64,
) -> bool {
    let token_bytes = token.as_bytes();
    // Recompute for the current minute and the previous one; a mismatched
    // length is itself public information (token lengths are not secret —
    // only the content is), so it is fine to branch on it before the
    // constant-time byte compare.
    [now_unix_minute, now_unix_minute - 1]
        .into_iter()
        .map(|minute| issue(key, from_workspace, from_project, from_path, minute))
        .any(|expected| {
            let expected = expected.as_bytes();
            expected.len() == token_bytes.len() && expected.ct_eq(token_bytes).into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> CsrfKey {
        CsrfKey::from_bytes([7u8; 32])
    }

    #[test]
    fn current_minute_token_verifies() {
        let k = key();
        let token = issue(&k, "ws", "proj", "p/foo.md", 1_000);
        assert!(verify(&k, &token, "ws", "proj", "p/foo.md", 1_000));
    }

    #[test]
    fn previous_minute_token_verifies_within_tolerance() {
        let k = key();
        let token = issue(&k, "ws", "proj", "p/foo.md", 1_000);
        assert!(verify(&k, &token, "ws", "proj", "p/foo.md", 1_001));
    }

    #[test]
    fn two_minutes_old_token_does_not_verify() {
        let k = key();
        let token = issue(&k, "ws", "proj", "p/foo.md", 1_000);
        assert!(!verify(&k, &token, "ws", "proj", "p/foo.md", 1_002));
    }

    #[test]
    fn token_signed_for_a_different_path_does_not_verify() {
        let k = key();
        let token = issue(&k, "ws", "proj", "p/foo.md", 1_000);
        assert!(!verify(&k, &token, "ws", "proj", "p/other.md", 1_000));
    }

    #[test]
    fn token_signed_for_a_different_workspace_or_project_does_not_verify() {
        let k = key();
        let token = issue(&k, "ws", "proj", "p/foo.md", 1_000);
        assert!(!verify(&k, &token, "other-ws", "proj", "p/foo.md", 1_000));
        assert!(!verify(&k, &token, "ws", "other-proj", "p/foo.md", 1_000));
    }

    #[test]
    fn token_signed_with_a_different_key_does_not_verify() {
        let k1 = key();
        let k2 = CsrfKey::from_bytes([9u8; 32]);
        let token = issue(&k1, "ws", "proj", "p/foo.md", 1_000);
        assert!(!verify(&k2, &token, "ws", "proj", "p/foo.md", 1_000));
    }

    #[test]
    fn garbage_token_does_not_verify() {
        let k = key();
        assert!(!verify(
            &k,
            "not-a-real-token",
            "ws",
            "proj",
            "p/foo.md",
            1_000
        ));
        assert!(!verify(&k, "", "ws", "proj", "p/foo.md", 1_000));
    }

    /// Boundary collision: without length-prefixing, `("ab", "c")` and
    /// `("a", "bc")` could sign identically for a naive concatenation.
    #[test]
    fn fields_do_not_collide_across_the_field_boundary() {
        let k = key();
        let token = issue(&k, "ab", "c", "p", 1_000);
        assert!(!verify(&k, &token, "a", "bc", "p", 1_000));
    }

    /// The framing collision a separator-based scheme is vulnerable to: a
    /// field that itself contains the separator byte. Under the old `\0`-
    /// joined scheme, `("a\0b", "c")` and `("a", "b\0c")` produced the exact
    /// same signed bytes (`a\0b\0c\0<path>\0<minute>`) — workspace/project
    /// names are arbitrary strings with no byte excluded, so this was a real
    /// forgery: a token issued for one (workspace, project) pair verified
    /// for a different one. Length-prefixing (see `signing_message`) closes
    /// this regardless of field content. This test fails against the old
    /// separator-joined `signing_message` and passes against the
    /// length-prefixed one.
    #[test]
    fn embedded_separator_byte_does_not_shift_the_workspace_project_boundary() {
        let k = key();
        let token = issue(&k, "a\0b", "c", "path", 1_000);
        assert!(!verify(&k, &token, "a", "b\0c", "path", 1_000));
    }
}

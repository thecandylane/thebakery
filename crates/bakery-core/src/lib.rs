//! bakery-core — the frozen event envelope, content/event hashing, and per-stream
//! chain verification for The Bakery's Knowledge Heart.
//!
//! This crate has ZERO dependencies on the rest of the workspace (locked boundary).
//!
//! ## Two hashes
//! - [`Event::content_hash`] = `blake3(payload)` — payload integrity (frozen envelope, literal).
//! - [`Event::event_hash`]   = `blake3(DOMAIN ‖ every envelope field)` — the chain link.
//!   `prev_hash` of the next event points at this. Chaining on the full envelope means
//!   tampering with `actor` / `ts` / `family` also breaks the chain, not just payload edits.
//!
//! `event_hash` is an additive field beyond KNOWLEDGE_HEART §3.2 and is pending sign-off.

use serde::{Deserialize, Serialize};

/// Domain-separation tag mixed into every `event_hash`. Bump the suffix if the
/// hashing scheme ever changes so old and new hashes can never collide.
pub const EVENT_HASH_DOMAIN: &[u8] = b"bakery.event.hash.v1";

/// A sealed, immutable event. Field set mirrors KNOWLEDGE_HEART §3.2 (the frozen
/// envelope) plus `event_hash` (see module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Scope this event belongs to: a worktree id, or `"factory"`.
    pub stream_id: String,
    /// Per-stream monotonic sequence, contiguous from 0.
    pub seq: u64,
    /// `blake3(payload)`, hex.
    pub content_hash: String,
    /// `event_hash` of the previous event in this stream. `None` for the first event.
    pub prev_hash: Option<String>,
    /// human | agent:<id> | indexer | baker
    pub actor: String,
    /// RFC3339 timestamp supplied by the owning node's clock.
    pub ts: String,
    /// Zero or more cross-stream `event_hash` references (the causality DAG).
    pub causality: Vec<String>,
    /// L0..L4 where applicable.
    pub layer: Option<String>,
    /// Sensitivity tag driving encryption/redaction. Defaults to `"normal"`.
    pub sensitivity: String,
    /// Event family, e.g. `project.onboarded`, `agent.file.changed`.
    pub family: String,
    /// Canonical payload bytes as a UTF-8 string (JSON text in practice).
    pub payload: String,
    /// `blake3(DOMAIN ‖ all fields above)`, hex. The chain link.
    pub event_hash: String,
}

/// Everything needed to seal a new event *except* the stream position, which the
/// store assigns atomically.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub stream_id: String,
    pub actor: String,
    pub ts: String,
    pub causality: Vec<String>,
    pub layer: Option<String>,
    pub sensitivity: String,
    pub family: String,
    pub payload: String,
}

impl NewEvent {
    /// Seal into an immutable [`Event`] at the given stream position.
    ///
    /// `seq` and `prev_hash` come from the store (the current stream tip); the
    /// store is responsible for supplying a contiguous `seq` and the matching
    /// `prev_hash`. Hashes are computed here so a sealed event is always internally
    /// consistent by construction.
    pub fn seal(self, seq: u64, prev_hash: Option<String>) -> Event {
        let content_hash = hash_payload(&self.payload);
        let event_hash = compute_event_hash(
            &self.stream_id,
            seq,
            &content_hash,
            prev_hash.as_deref(),
            &self.actor,
            &self.ts,
            &self.causality,
            self.layer.as_deref(),
            &self.sensitivity,
            &self.family,
        );
        Event {
            stream_id: self.stream_id,
            seq,
            content_hash,
            prev_hash,
            actor: self.actor,
            ts: self.ts,
            causality: self.causality,
            layer: self.layer,
            sensitivity: self.sensitivity,
            family: self.family,
            payload: self.payload,
            event_hash,
        }
    }
}

impl Event {
    /// Recompute `content_hash` from the current payload.
    pub fn recompute_content_hash(&self) -> String {
        hash_payload(&self.payload)
    }

    /// Recompute `event_hash` from all current envelope fields.
    pub fn recompute_event_hash(&self) -> String {
        compute_event_hash(
            &self.stream_id,
            self.seq,
            &self.content_hash,
            self.prev_hash.as_deref(),
            &self.actor,
            &self.ts,
            &self.causality,
            self.layer.as_deref(),
            &self.sensitivity,
            &self.family,
        )
    }

    /// True iff both stored hashes match a fresh recomputation (i.e. no field
    /// has been mutated since sealing). Does NOT check chain linkage — see
    /// [`verify_chain`].
    pub fn verify_self(&self) -> bool {
        self.content_hash == self.recompute_content_hash()
            && self.event_hash == self.recompute_event_hash()
    }
}

/// `blake3(payload)` as a lowercase hex string.
pub fn hash_payload(payload: &str) -> String {
    blake3::hash(payload.as_bytes()).to_hex().to_string()
}

/// The full-envelope chain hash. Every field is fed with an unambiguous,
/// length-prefixed, domain-separated encoding so that no two distinct field
/// sets can ever produce the same digest (no boundary-confusion collisions).
#[allow(clippy::too_many_arguments)]
pub fn compute_event_hash(
    stream_id: &str,
    seq: u64,
    content_hash: &str,
    prev_hash: Option<&str>,
    actor: &str,
    ts: &str,
    causality: &[String],
    layer: Option<&str>,
    sensitivity: &str,
    family: &str,
) -> String {
    let mut h = blake3::Hasher::new();
    h.update(EVENT_HASH_DOMAIN);
    feed(&mut h, stream_id.as_bytes());
    h.update(&seq.to_le_bytes());
    feed(&mut h, content_hash.as_bytes());
    feed_opt(&mut h, prev_hash.map(str::as_bytes));
    feed(&mut h, actor.as_bytes());
    feed(&mut h, ts.as_bytes());
    // causality: length-prefixed count, then each length-prefixed element.
    h.update(&(causality.len() as u64).to_le_bytes());
    for c in causality {
        feed(&mut h, c.as_bytes());
    }
    feed_opt(&mut h, layer.map(str::as_bytes));
    feed(&mut h, sensitivity.as_bytes());
    feed(&mut h, family.as_bytes());
    h.finalize().to_hex().to_string()
}

/// Feed one field as: u64 little-endian length, then the bytes.
fn feed(h: &mut blake3::Hasher, field: &[u8]) {
    h.update(&(field.len() as u64).to_le_bytes());
    h.update(field);
}

/// Feed an optional field as: a presence byte (0 or 1), then (if present) the field.
fn feed_opt(h: &mut blake3::Hasher, field: Option<&[u8]>) {
    match field {
        Some(bytes) => {
            h.update(&[1u8]);
            feed(h, bytes);
        }
        None => {
            h.update(&[0u8]);
        }
    }
}

/// Why a stream failed chain verification. `index` is the position in the slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainError {
    /// `seq` is not contiguous from 0. Expected vs found.
    SeqGap { index: usize, expected: u64, found: u64 },
    /// The first event carries a `prev_hash` (it must not) or a non-first event
    /// carries `None`.
    PrevHashShape { index: usize },
    /// A non-first event's `prev_hash` does not equal the previous event's `event_hash`.
    BrokenLink { index: usize },
    /// `content_hash` does not match `blake3(payload)` — payload was tampered.
    ContentHashMismatch { index: usize },
    /// `event_hash` does not match a recomputation — some envelope field was tampered.
    EventHashMismatch { index: usize },
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainError::SeqGap { index, expected, found } => {
                write!(f, "seq gap at index {index}: expected {expected}, found {found}")
            }
            ChainError::PrevHashShape { index } => {
                write!(f, "prev_hash shape wrong at index {index}")
            }
            ChainError::BrokenLink { index } => write!(f, "broken chain link at index {index}"),
            ChainError::ContentHashMismatch { index } => {
                write!(f, "content_hash mismatch at index {index}")
            }
            ChainError::EventHashMismatch { index } => {
                write!(f, "event_hash mismatch at index {index}")
            }
        }
    }
}

impl std::error::Error for ChainError {}

/// Verify a full stream, given in ascending `seq` order. Checks, for every event:
/// contiguous seq from 0, correct prev_hash shape/linkage, and both self-hashes.
/// An empty slice verifies (vacuously).
pub fn verify_chain(events: &[Event]) -> Result<(), ChainError> {
    for (i, e) in events.iter().enumerate() {
        let idx = i as u64;
        if e.seq != idx {
            return Err(ChainError::SeqGap { index: i, expected: idx, found: e.seq });
        }
        if e.content_hash != e.recompute_content_hash() {
            return Err(ChainError::ContentHashMismatch { index: i });
        }
        if e.event_hash != e.recompute_event_hash() {
            return Err(ChainError::EventHashMismatch { index: i });
        }
        match (i, &e.prev_hash) {
            (0, None) => {}
            (0, Some(_)) => return Err(ChainError::PrevHashShape { index: i }),
            (_, None) => return Err(ChainError::PrevHashShape { index: i }),
            (_, Some(prev)) => {
                if prev != &events[i - 1].event_hash {
                    return Err(ChainError::BrokenLink { index: i });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(stream: &str, family: &str, payload: &str) -> NewEvent {
        NewEvent {
            stream_id: stream.to_string(),
            actor: "human".to_string(),
            ts: "2026-07-23T00:00:00Z".to_string(), // fixed → deterministic hashes
            causality: vec![],
            layer: Some("L2".to_string()),
            sensitivity: "normal".to_string(),
            family: family.to_string(),
            payload: payload.to_string(),
        }
    }

    fn chain(stream: &str, payloads: &[&str]) -> Vec<Event> {
        let mut out: Vec<Event> = Vec::new();
        for (i, p) in payloads.iter().enumerate() {
            let prev = out.last().map(|e| e.event_hash.clone());
            out.push(sample(stream, "test.event", p).seal(i as u64, prev));
        }
        out
    }

    #[test]
    fn content_hash_matches_blake3_of_payload() {
        let e = sample("s", "f", "hello").seal(0, None);
        assert_eq!(e.content_hash, blake3::hash(b"hello").to_hex().to_string());
    }

    #[test]
    fn sealing_is_deterministic() {
        let a = sample("s", "f", "same").seal(3, Some("deadbeef".to_string()));
        let b = sample("s", "f", "same").seal(3, Some("deadbeef".to_string()));
        assert_eq!(a.event_hash, b.event_hash);
        assert_eq!(a.content_hash, b.content_hash);
    }

    #[test]
    fn sealed_event_verifies_self() {
        assert!(sample("s", "f", "x").seal(0, None).verify_self());
    }

    #[test]
    fn payload_tamper_breaks_content_and_event_hash() {
        let mut e = sample("s", "f", "original").seal(0, None);
        e.payload = "tampered".to_string();
        assert!(!e.verify_self());
        assert_ne!(e.content_hash, e.recompute_content_hash());
    }

    #[test]
    fn metadata_tamper_breaks_event_hash() {
        // The whole reason event_hash chains the full envelope, not just payload.
        let mut e = sample("s", "f", "x").seal(0, None);
        assert!(e.verify_self());
        e.actor = "attacker".to_string();
        assert!(!e.verify_self());
        // content_hash still matches (payload untouched) — only event_hash catches it.
        assert_eq!(e.content_hash, e.recompute_content_hash());
        assert_ne!(e.event_hash, e.recompute_event_hash());
    }

    #[test]
    fn distinct_fields_produce_distinct_hashes() {
        let a = sample("s", "f", "x").seal(0, None);
        let b = sample("s", "g", "x").seal(0, None); // different family only
        assert_ne!(a.event_hash, b.event_hash);
    }

    #[test]
    fn length_prefix_prevents_boundary_collision() {
        // Without length-prefixing, ("ab","c") and ("a","bc") could collide.
        let h1 = compute_event_hash("ab", 0, "c", None, "x", "t", &[], None, "n", "f");
        let h2 = compute_event_hash("a", 0, "bc", None, "x", "t", &[], None, "n", "f");
        assert_ne!(h1, h2);
    }

    #[test]
    fn good_chain_verifies() {
        assert!(verify_chain(&chain("s", &["a", "b", "c"])).is_ok());
    }

    #[test]
    fn empty_chain_verifies() {
        assert!(verify_chain(&[]).is_ok());
    }

    #[test]
    fn payload_tamper_detected_by_chain() {
        let mut c = chain("s", &["a", "b", "c"]);
        c[1].payload = "evil".to_string();
        assert_eq!(verify_chain(&c), Err(ChainError::ContentHashMismatch { index: 1 }));
    }

    #[test]
    fn relinking_reseals_but_breaks_downstream() {
        // Attacker rewrites event 1's payload AND re-seals event 1 to hide the
        // content/event hash mismatch. The link from event 2 still points at the
        // ORIGINAL event_hash, so the chain breaks at index 2.
        let mut c = chain("s", &["a", "b", "c"]);
        let resealed = sample("s", "test.event", "evil").seal(1, c[0].event_hash.clone().into());
        c[1] = resealed;
        assert_eq!(verify_chain(&c), Err(ChainError::BrokenLink { index: 2 }));
    }

    #[test]
    fn first_event_with_prev_hash_rejected() {
        let mut c = chain("s", &["a"]);
        c[0] = sample("s", "test.event", "a").seal(0, Some("bogus".to_string()));
        assert_eq!(verify_chain(&c), Err(ChainError::PrevHashShape { index: 0 }));
    }

    #[test]
    fn seq_gap_detected() {
        let mut c = chain("s", &["a", "b"]);
        c[1].seq = 5;
        // seq is checked before hashes; mutating seq also invalidates event_hash,
        // but the seq check fires first.
        assert_eq!(verify_chain(&c), Err(ChainError::SeqGap { index: 1, expected: 1, found: 5 }));
    }

    #[test]
    fn empty_payload_is_valid() {
        let e = sample("s", "f", "").seal(0, None);
        assert!(e.verify_self());
        assert_eq!(e.content_hash, blake3::hash(b"").to_hex().to_string());
    }
}

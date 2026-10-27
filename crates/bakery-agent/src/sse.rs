//! Shared SSE (Server-Sent Events) framing plus a cancel/idle/hard-cap-aware
//! control loop, reused by every streaming provider (07d-A5:
//! `claude.rs`/`grok.rs`/`gemini.rs`/`openai_compat.rs`). Nothing in this
//! module knows about any provider's JSON payload shape — turning one
//! provider's stream of [`SseEvent`]s into a [`crate::ChatReply`] is
//! `tool_wire.rs`'s job, fed incrementally instead of once.
//!
//! Cancellation cadence is the one subtle correctness property here,
//! verified directly against `ureq` 2.12.1's own source rather than
//! guessed: if an overall request `.timeout(...)` is set (as every
//! non-streaming call in this crate already does, 120s), `ureq`'s
//! `DeadlineStream` re-applies "time remaining until that ONE deadline" as
//! the socket's read timeout on every single read — useless for polling
//! cancellation early in a long response. [`streaming_agent`] deliberately
//! builds a dedicated agent with NO overall timeout and a short per-read
//! idle timeout instead, so every blocking read individually times out at a
//! fixed, small cadence regardless of how much of the response has already
//! arrived — see [`IDLE_READ_TIMEOUT`].
//!
//! `SseReader` reads via `BufRead::fill_buf`/`consume` directly, never
//! `read_line`/`read_until` — this matters for two independent reasons,
//! both found by review after an earlier version of this file used
//! `read_until`:
//!
//! 1. **Bounding a single unterminated line.** `read_until` has no size
//!    bound of its own: its stdlib implementation keeps calling `fill_buf`/
//!    `consume` internally, accumulating into the caller's `Vec<u8>`, for as
//!    long as no delimiter is found and the underlying reads keep
//!    succeeding — it only returns on the delimiter, EOF, or a hard error,
//!    never merely because a lot of bytes have piled up. A peer sending one
//!    endless (or very long) "line" with no `\n` anywhere, as long as it
//!    supplies at least one byte more often than [`IDLE_READ_TIMEOUT`], kept
//!    a single `read_until` call from ever returning — so [`Self::next_step`]
//!    never got a chance to check its own byte/line caps, and [`drive`]'s
//!    cancel/hard-cap checks (which only run *between* `next_step` calls)
//!    were starved indefinitely. `fill_buf` has no such internal loop: one
//!    call returns at most whatever is already buffered, or triggers *one*
//!    underlying read — so calling it in an explicit loop here, checking the
//!    byte/line caps after every single call (not just after a full line),
//!    bounds a pathological unterminated line to a small, fixed number of
//!    iterations regardless of how long the peer keeps it up.
//! 2. **Never losing bytes across a timed-out read.** `BufRead::read_line`'s
//!    `String`-based API maintains "the buffer is always valid UTF-8" as an
//!    invariant, so on an I/O error mid-read (a timeout landing inside a
//!    multi-byte character split across two socket reads) it silently rolls
//!    the `String` back to its last valid boundary — discarding the very
//!    bytes it just read, then hard-failing the *next* read once it observes
//!    a truncated sequence it cannot complete. Reading raw bytes via
//!    `fill_buf`/`consume` into a persistent `Vec<u8>` field has no such
//!    invariant to protect, so nothing is ever rolled back: whatever was
//!    consumed stays in the buffer, and UTF-8 decoding (`String::from_utf8_lossy`)
//!    happens exactly once, only once a complete line has been assembled.
//!
//! **A known, narrower residual limitation**, not fixed by the above: every
//! provider constructs `SseReader` over `ureq::Response::into_reader()`
//! (`ureq` 2.12.1), and every real streaming completion from these APIs
//! arrives `Transfer-Encoding: chunked` (no upfront `Content-Length`).
//! `ureq` wraps that as its own internal `ChunkDecoder` sitting *underneath*
//! the `BufReader` this module controls — the byte-preservation guarantee
//! above only covers what this module's own `fill_buf`/`consume` loop does;
//! it says nothing about `ureq`'s internal chunk-size-line parsing, which
//! reads its few ASCII hex digits one byte at a time into a function-local
//! buffer that is *not* preserved across an I/O error. If `IDLE_READ_TIMEOUT`
//! (1s) fires at the exact byte offset partway through a chunk-size line — a
//! narrow window in practice, since that line is only a handful of bytes and
//! ordinary TCP delivery rarely fragments precisely inside it — `ureq`'s own
//! decoder can silently desynchronize from the true chunk boundaries.
//! Fully closing this would mean bypassing `ureq`'s automatic dechunking and
//! implementing chunked-transfer-encoding parsing directly in this crate —
//! a much larger change than this module's own scope, not undertaken here.
//! The failure modes when this narrow window is actually hit are *expected*
//! to degrade into paths this module and `tool_wire.rs` already handle
//! deliberately — misaligned bytes most plausibly fail JSON parsing in a
//! provider assembler (an existing `Abort`/`Err` path) or surface as a
//! [`DriveOutcome::IoError`] once the framing desyncs enough that `ureq`
//! itself returns a decode error — but this is a fail-closed *expectation*,
//! not a proof: a misparsed chunk size could, in principle, still land on
//! bytes that happen to parse as something structurally valid-looking. The
//! real fix is replacing `ureq`'s streaming path with a transport whose own
//! timeout/framing behavior this crate controls end to end, rather than
//! composing on top of a black-box decoder's internal state; that is
//! tracked as follow-up work, not attempted here.
//!
//! [`STREAM_HARD_CAP`] is tracked against [`SseReader::last_progress_at`],
//! refreshed on every `fill_buf` call that returns real bytes — not merely
//! whenever a full [`SseEvent`] completes. A legitimate slow-but-alive
//! stream can take longer than the hard cap to finish assembling a single
//! record (many small reads, each well within [`IDLE_READ_TIMEOUT`], with
//! long real gaps between them); tracking only completed events would
//! misdiagnose that as a dead connection and kill it with a factually wrong
//! "no new data" message even though bytes kept arriving throughout.

use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// One parsed SSE record: an optional `event:` name plus the concatenated
/// `data:` field(s) up to the next blank line (WHATWG SSE framing — the
/// same framing Anthropic, the OpenAI-compatible shape, and Gemini's
/// `alt=sse` all use; only the *payload* shape and end-of-stream signal
/// differ per provider, handled in `tool_wire.rs`, not here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// One step of reading.
#[derive(Debug)]
pub enum SseStep {
    Event(SseEvent),
    /// One read attempt returned no data within [`IDLE_READ_TIMEOUT`] — NOT
    /// end-of-stream. The caller polls `cancel` and its own wall-clock cap,
    /// then calls `next_step` again.
    Idle,
    /// Real bytes arrived and were processed (consumed into the in-progress
    /// record, or a non-terminating line was parsed), but no complete
    /// [`SseEvent`] is ready yet. Deliberately distinct from looping
    /// internally to keep processing: a peer that trickles single bytes
    /// (or single short lines) just under [`IDLE_READ_TIMEOUT`] apart,
    /// never emitting the record terminator, would otherwise never let
    /// [`next_step`] return at all — which would starve [`drive`]'s
    /// cancel/hard-cap checks indefinitely even though every individual
    /// read stays well within the byte/line caps. Returning `Progress`
    /// after every single unit of work (one `fill_buf` call, or one parsed
    /// line) guarantees the caller regains control between every read, not
    /// just between complete records.
    Progress,
    /// Clean EOF. Whether this is a normal completion or a stream that died
    /// mid-flight is a protocol-specific judgment only the caller (in
    /// `tool_wire.rs`) can make — Gemini's protocol ends exactly this way
    /// with no separate terminator record; Claude/the OpenAI-compatible
    /// shape both have their own explicit completion signal, so an `Eof`
    /// before that signal arrived means the connection was lost.
    Eof,
}

/// Every read on a streaming connection blocks for at most this long before
/// giving the caller a chance to check cancellation and its own hard cap —
/// see the module doc for why this requires a dedicated `ureq::Agent`
/// ([`streaming_agent`]) rather than the default one non-streaming calls
/// share.
pub const IDLE_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Wall-clock ceiling on how long a stream may go without producing a
/// single new byte — a backstop against a connection that never explicitly
/// closes and whose caller never sets `cancel`. Deliberately generous (real
/// "the model is thinking" gaps are normal and much shorter than this).
/// Measured against [`SseReader::last_progress_at`] — see the module doc.
pub const STREAM_HARD_CAP: Duration = Duration::from_secs(300);

/// Cumulative bytes a single in-progress SSE record (the lines between two
/// blank-line terminators) may accumulate before `next_step` gives up and
/// returns an error. A single-shot response body is bounded by its
/// `Content-Length` (or, for the openai-compat endpoint, the explicit
/// `MAX_RESPONSE_BYTES` bounded reader); a stream has no such bound up
/// front, so this is the streaming equivalent — and doubles as the backstop
/// against a peer that never sends the blank-line terminator at all (or
/// even a single `\n`), which would otherwise let `next_step`'s loop run
/// forever. Checked incrementally, after every underlying read — not just
/// once a full line is assembled — so a single pathologically long,
/// never-terminated line is bounded too (see the module doc).
pub const MAX_STREAMED_TEXT_BYTES: usize = 4 * 1024 * 1024;

/// Independent backstop alongside [`MAX_STREAMED_TEXT_BYTES`]: a peer that
/// floods blank keepalive lines forever (no `data:`/`event:` content at
/// all) would never grow the byte count, so line count is capped
/// separately. Generous — real SSE traffic never comes close.
pub const MAX_LINES_PER_RECORD: usize = 100_000;

/// Turns a byte stream into a sequence of [`SseEvent`]s. Holds in-progress
/// record/line state as fields (not locals) so a timed-out read that
/// returns [`SseStep::Idle`] never loses partially-read bytes — the next
/// call to [`Self::next_step`] resumes exactly where the last one left off.
pub struct SseReader<R> {
    reader: BufReader<R>,
    event: Option<String>,
    data_lines: Vec<String>,
    partial_line: Vec<u8>,
    record_bytes: usize,
    record_lines: usize,
    max_record_bytes: usize,
    max_record_lines: usize,
    /// Refreshed on every underlying read that returns real bytes (see the
    /// module doc) — what [`drive`] measures [`STREAM_HARD_CAP`] against.
    last_progress_at: Instant,
}

impl<R: Read> SseReader<R> {
    pub fn new(inner: R) -> Self {
        Self::with_limits(inner, MAX_STREAMED_TEXT_BYTES, MAX_LINES_PER_RECORD)
    }

    /// Same as [`Self::new`] but with overridable caps — used by tests that
    /// need to prove the cap-enforcement path fires without actually
    /// pushing megabytes/hundreds-of-thousands of lines through a unit
    /// test.
    fn with_limits(inner: R, max_record_bytes: usize, max_record_lines: usize) -> Self {
        Self {
            reader: BufReader::new(inner),
            event: None,
            data_lines: Vec::new(),
            partial_line: Vec::new(),
            record_bytes: 0,
            record_lines: 0,
            max_record_bytes,
            max_record_lines,
            last_progress_at: Instant::now(),
        }
    }

    /// See the module doc — [`drive`] (or [`drive_with_hard_cap`]) measures
    /// [`STREAM_HARD_CAP`] against this, not against the last completed
    /// [`SseEvent`].
    pub fn last_progress_at(&self) -> Instant {
        self.last_progress_at
    }

    /// Does at most one unit of work — one `fill_buf`/`consume` cycle, or
    /// (once a full line is ready) processing exactly that one line — and
    /// always returns immediately afterward, never looping internally to
    /// keep reading. This is the fix for a real regression a previous
    /// version of this method had: looping internally until a complete
    /// line/record was ready meant a peer trickling single bytes (or single
    /// short lines) just under [`IDLE_READ_TIMEOUT`] apart, but never
    /// emitting the record terminator, could keep `next_step` from ever
    /// returning at all — every individual read stayed within the byte/line
    /// caps, so those caps never tripped either, and [`drive`]'s cancel/
    /// hard-cap checks (which only run *between* calls to this method) were
    /// starved for as long as the peer kept it up. Returning [`SseStep::
    /// Progress`] after every single unit of work guarantees the caller
    /// regains control between every read, not just between complete
    /// records — see that variant's own doc for the full reasoning.
    ///
    /// A blank line closes out the in-progress record (SSE's own
    /// field-terminator rule); any other field name (`id:`, `retry:`, a
    /// `:`-prefixed comment) is silently ignored, matching every real
    /// provider's own tolerance for unknown SSE fields. Bounded by
    /// `max_record_bytes`/`max_record_lines` (see the module doc), checked
    /// after every single `fill_buf` call — not just once a full line is
    /// assembled — so neither a peer that never emits the blank-line
    /// terminator, nor one that never emits `\n` at all, can grow this
    /// without bound.
    pub fn next_step(&mut self) -> std::io::Result<SseStep> {
        let buf = match self.reader.fill_buf() {
            Ok(b) => b,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Ok(SseStep::Idle);
            }
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            return Ok(SseStep::Eof);
        }
        self.last_progress_at = Instant::now();

        let newline_at = buf.iter().position(|&b| b == b'\n');
        let take = newline_at.map(|p| p + 1).unwrap_or(buf.len());
        self.partial_line.extend_from_slice(&buf[..take]);
        self.record_bytes += take;
        self.reader.consume(take);

        if self.record_bytes > self.max_record_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("SSE record exceeded {} bytes without a terminating blank line", self.max_record_bytes),
            ));
        }

        if newline_at.is_none() {
            // No complete line yet from this one read — return control to
            // the caller instead of looping internally.
            return Ok(SseStep::Progress);
        }

        self.record_lines += 1;
        if self.record_lines > self.max_record_lines {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("SSE record exceeded {} lines without a terminating blank line", self.max_record_lines),
            ));
        }

        let line = std::mem::take(&mut self.partial_line);
        let decoded = String::from_utf8_lossy(&line);
        let trimmed = decoded.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if self.data_lines.is_empty() && self.event.is_none() {
                // A blank keepalive line with nothing buffered yet — also
                // return control rather than looping internally.
                return Ok(SseStep::Progress);
            }
            let event = self.event.take();
            let data = self.data_lines.join("\n");
            self.data_lines.clear();
            self.record_bytes = 0;
            self.record_lines = 0;
            return Ok(SseStep::Event(SseEvent { event, data }));
        }
        if let Some(rest) = trimmed.strip_prefix("data:") {
            self.data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        } else if let Some(rest) = trimmed.strip_prefix("event:") {
            self.event = Some(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
        // Any other field — ignored, but still just one processed line.
        Ok(SseStep::Progress)
    }
}

/// A dedicated, lazily-built streaming `ureq::Agent` — see the module doc
/// for why this must never be the default agent the non-streaming `ureq::
/// get`/`ureq::post` calls implicitly share.
pub fn streaming_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(IDLE_READ_TIMEOUT)
            .build()
    })
}

/// What a per-event callback tells [`drive`] to do next.
pub enum DriveControl {
    Continue,
    /// A normal, protocol-recognized completion signal was seen.
    Stop,
    /// The stream is structurally broken (a chunk that doesn't parse at
    /// all, an explicit provider-sent error record, or the accumulated text
    /// cap was exceeded) — distinct from an ambiguous connection death.
    Abort(String),
}

pub enum DriveOutcome {
    /// `on_event` returned `Stop` — a real completion signal was observed.
    FinishedNormally,
    Cancelled,
    /// Clean EOF with no `Stop`/`Abort` ever having fired — the caller
    /// decides whether this is fine (Gemini) or a stream death (Claude/
    /// OpenAI-shaped, which both have their own explicit terminator).
    Eof,
    IoError(String),
}

/// The one control-flow loop every streaming provider reuses instead of
/// reimplementing cancel/idle/hard-cap interplay four times. Checks `cancel`
/// both before each blocking read attempt and again before dispatching each
/// parsed event (a single already-buffered `read()` can surface several
/// events at once — checking only at the syscall level would let a whole
/// burst flow through even after cancellation was requested mid-burst).
pub fn drive<R: Read>(
    reader: &mut SseReader<R>,
    cancel: &AtomicBool,
    on_event: impl FnMut(SseEvent) -> DriveControl,
) -> DriveOutcome {
    drive_with_hard_cap(reader, cancel, STREAM_HARD_CAP, on_event)
}

/// [`drive`]'s real implementation, with an overridable hard cap — used by
/// tests that need a hard cap small enough to actually observe firing (or
/// not firing) within a fast unit test, without waiting out the real 300s
/// [`STREAM_HARD_CAP`].
fn drive_with_hard_cap<R: Read>(
    reader: &mut SseReader<R>,
    cancel: &AtomicBool,
    hard_cap: Duration,
    mut on_event: impl FnMut(SseEvent) -> DriveControl,
) -> DriveOutcome {
    loop {
        if cancel.load(Ordering::Acquire) {
            return DriveOutcome::Cancelled;
        }
        if reader.last_progress_at().elapsed() > hard_cap {
            return DriveOutcome::IoError("stream exceeded its hard time cap with no new data".to_string());
        }
        match reader.next_step() {
            Ok(SseStep::Idle) => continue,
            Ok(SseStep::Progress) => continue,
            Ok(SseStep::Eof) => return DriveOutcome::Eof,
            Ok(SseStep::Event(ev)) => {
                if cancel.load(Ordering::Acquire) {
                    return DriveOutcome::Cancelled;
                }
                match on_event(ev) {
                    DriveControl::Continue => continue,
                    DriveControl::Stop => return DriveOutcome::FinishedNormally,
                    DriveControl::Abort(msg) => return DriveOutcome::IoError(msg),
                }
            }
            Err(e) => return DriveOutcome::IoError(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    /// `next_step` now returns [`SseStep::Progress`] after every single
    /// unit of work (see its doc), so a test that just wants "the next
    /// real event" loops past `Progress` the same way [`drive`] does in
    /// production. Tests that specifically want to observe `Progress`/
    /// `Idle` as distinct steps call `next_step` directly instead.
    fn next_event<R: Read>(r: &mut SseReader<R>) -> std::io::Result<SseStep> {
        loop {
            match r.next_step()? {
                SseStep::Progress => continue,
                other => return Ok(other),
            }
        }
    }

    #[test]
    fn parses_multi_line_data_and_a_named_event() {
        let body = "event: content_block_delta\ndata: {\"a\":1}\n\ndata: line one\ndata: line two\n\n";
        let mut r = SseReader::new(Cursor::new(body.as_bytes().to_vec()));
        let SseStep::Event(ev) = next_event(&mut r).unwrap() else { panic!("expected an event") };
        assert_eq!(ev.event.as_deref(), Some("content_block_delta"));
        assert_eq!(ev.data, "{\"a\":1}");

        let SseStep::Event(ev2) = next_event(&mut r).unwrap() else { panic!("expected an event") };
        assert_eq!(ev2.event, None, "a record with no event: line has no name");
        assert_eq!(ev2.data, "line one\nline two", "multiple data: lines join with \\n");

        assert!(matches!(next_event(&mut r).unwrap(), SseStep::Eof));
    }

    #[test]
    fn ignores_unknown_fields_and_blank_keepalives() {
        let body = ": a comment line\nid: 5\nretry: 1000\n\ndata: hello\n\n";
        let mut r = SseReader::new(Cursor::new(body.as_bytes().to_vec()));
        let SseStep::Event(ev) = next_event(&mut r).unwrap() else { panic!("expected an event") };
        assert_eq!(ev.data, "hello");
    }

    /// A `Read` that yields bytes one at a time, split across a
    /// caller-scripted sequence of `Ok`/`WouldBlock` results — proves a
    /// timeout mid-record never loses already-buffered bytes.
    struct ScriptedReader {
        chunks: std::collections::VecDeque<Result<Vec<u8>, std::io::ErrorKind>>,
    }
    impl Read for ScriptedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.chunks.pop_front() {
                None => Ok(0),
                Some(Err(kind)) => Err(std::io::Error::new(kind, "scripted timeout")),
                Some(Ok(bytes)) => {
                    let n = bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&bytes[..n]);
                    Ok(n)
                }
            }
        }
    }

    #[test]
    fn a_timeout_mid_record_does_not_lose_already_buffered_bytes() {
        let mut chunks = std::collections::VecDeque::new();
        chunks.push_back(Ok(b"data: par".to_vec()));
        chunks.push_back(Err(std::io::ErrorKind::WouldBlock));
        chunks.push_back(Ok(b"tial\n\n".to_vec()));
        let mut r = SseReader::new(ScriptedReader { chunks });

        // First call consumes "data: par" (no newline yet) — Progress.
        assert!(matches!(r.next_step().unwrap(), SseStep::Progress));
        // Second call hits the scripted WouldBlock — Idle.
        assert!(matches!(r.next_step().unwrap(), SseStep::Idle), "the WouldBlock read must surface as Idle");
        // Third call resumes exactly where it left off and completes the event.
        let SseStep::Event(ev) = next_event(&mut r).unwrap() else { panic!("expected an event") };
        assert_eq!(ev.data, "partial", "bytes read before the timeout must not be discarded");
    }

    /// The exact repro from the 07d-A5 review: a timeout lands in the
    /// middle of a multi-byte UTF-8 character (here, "é" = 0xC3 0xA9,
    /// split after the first byte). Bytes read via `fill_buf`/`consume`
    /// into a persistent `Vec<u8>` are never rolled back on error, so this
    /// must transparently reassemble on the next call.
    #[test]
    fn a_timeout_splitting_a_multi_byte_utf8_character_reassembles_correctly() {
        let mut chunks = std::collections::VecDeque::new();
        chunks.push_back(Ok(b"data: caf\xC3".to_vec())); // "caf" + first byte of "é"
        chunks.push_back(Err(std::io::ErrorKind::WouldBlock));
        chunks.push_back(Ok(b"\xA9\n\n".to_vec())); // second byte of "é", then terminator
        let mut r = SseReader::new(ScriptedReader { chunks });

        assert!(matches!(r.next_step().unwrap(), SseStep::Progress));
        assert!(matches!(r.next_step().unwrap(), SseStep::Idle));
        let SseStep::Event(ev) = next_event(&mut r).unwrap() else { panic!("expected an event, not an error") };
        assert_eq!(ev.data, "café", "the split multi-byte character must reassemble, not be lost or hard-fail");
    }

    #[test]
    fn drive_stops_dedup_on_a_recognized_completion_signal() {
        let body = "data: one\n\ndata: two\n\ndata: [DONE]\n\ndata: three\n\n";
        let mut r = SseReader::new(Cursor::new(body.as_bytes().to_vec()));
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let outcome = drive(&mut r, &cancel, |ev| {
            if ev.data == "[DONE]" {
                DriveControl::Stop
            } else {
                seen.push(ev.data);
                DriveControl::Continue
            }
        });
        assert!(matches!(outcome, DriveOutcome::FinishedNormally));
        assert_eq!(seen, vec!["one", "two"], "the record after the terminator must never be dispatched");
    }

    #[test]
    fn drive_stops_as_soon_as_cancel_is_set_even_mid_burst() {
        let body = "data: one\n\ndata: two\n\ndata: three\n\ndata: four\n\n";
        let mut r = SseReader::new(Cursor::new(body.as_bytes().to_vec()));
        let cancel = AtomicBool::new(false);
        let mut count = 0u32;
        let outcome = drive(&mut r, &cancel, |_ev| {
            count += 1;
            if count == 2 {
                cancel.store(true, Ordering::Release);
            }
            DriveControl::Continue
        });
        assert!(matches!(outcome, DriveOutcome::Cancelled));
        assert_eq!(count, 2, "must stop right after the event that set cancel, never dispatch a third");
    }

    #[test]
    fn drive_reports_eof_when_no_terminator_was_ever_seen() {
        let body = "data: only one chunk\n\n";
        let mut r = SseReader::new(Cursor::new(body.as_bytes().to_vec()));
        let cancel = AtomicBool::new(false);
        let outcome = drive(&mut r, &cancel, |_ev| DriveControl::Continue);
        assert!(matches!(outcome, DriveOutcome::Eof));
    }

    /// A `Read` that legitimately never returns data — proves `drive`
    /// remains responsive to `cancel` even while permanently Idle.
    struct AlwaysIdleReader;
    impl Read for AlwaysIdleReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "never any data"))
        }
    }

    #[test]
    fn drive_stays_responsive_to_cancel_while_permanently_idle() {
        let mut r = SseReader::new(AlwaysIdleReader);
        let cancel = AtomicBool::new(true); // pre-cancelled: must return immediately regardless of Idle
        let outcome = drive(&mut r, &cancel, |_ev| DriveControl::Continue);
        assert!(matches!(outcome, DriveOutcome::Cancelled));
    }

    /// Exercises the hard-cap *timing* mechanism directly (the test above
    /// only proves cancel precedence) — a connection producing literally
    /// no bytes must eventually trip a real, small hard cap rather than
    /// spin forever.
    #[test]
    fn drive_with_hard_cap_fires_for_a_connection_that_never_sends_anything() {
        let mut r = SseReader::new(AlwaysIdleReader);
        let cancel = AtomicBool::new(false);
        let outcome = drive_with_hard_cap(&mut r, &cancel, Duration::from_millis(10), |_ev| DriveControl::Continue);
        assert!(matches!(outcome, DriveOutcome::IoError(_)));
    }

    /// A `Read` that dribbles real bytes with a short real delay between
    /// each, never completing a full SSE record until the very last chunk.
    struct DribblingReader {
        chunks: std::collections::VecDeque<&'static [u8]>,
        delay: Duration,
    }
    impl Read for DribblingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.chunks.pop_front() {
                None => Ok(0),
                Some(bytes) => {
                    std::thread::sleep(self.delay);
                    let n = bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&bytes[..n]);
                    Ok(n)
                }
            }
        }
    }

    /// 07d-A5 fix-verification review: `drive`'s hard cap must track time
    /// since the last REAL byte, not time since the last completed
    /// `SseEvent` — a legitimate slow-but-alive stream whose one record
    /// takes longer than the cap to complete (each individual gap short,
    /// but the sum long) must not be misdiagnosed as dead.
    #[test]
    fn drive_does_not_falsely_trip_the_hard_cap_against_a_slow_but_genuinely_alive_stream() {
        let chunks: std::collections::VecDeque<&'static [u8]> =
            vec![b"data: a".as_slice(), b"b".as_slice(), b"c".as_slice(), b"d\n\n".as_slice()].into();
        // Four ~60ms gaps (~240ms total) against a 100ms hard cap: each
        // individual gap is well under the cap, but the total is not — the
        // old (Event-only) refresh logic would have wrongly killed this
        // partway through, since no Event completes until the last chunk.
        let mut r = SseReader::new(DribblingReader { chunks, delay: Duration::from_millis(60) });
        let cancel = AtomicBool::new(false);
        let outcome = drive_with_hard_cap(&mut r, &cancel, Duration::from_millis(100), |_ev| DriveControl::Stop);
        assert!(
            matches!(outcome, DriveOutcome::FinishedNormally),
            "real, continuing per-read progress must keep resetting the hard-cap timer"
        );
    }

    /// A `Read` that yields exactly one byte per call, after a short real
    /// delay, and never a `\n` — the exact repro for the second
    /// fix-verification round's blocker: a peer trickling single bytes just
    /// under `IDLE_READ_TIMEOUT` apart, with no newline, would keep
    /// `next_step`'s (then-internal) loop from ever returning at all,
    /// starving `drive`'s cancel check for as long as the peer kept it up
    /// (independent of, and reachable well before, the byte cap).
    struct SlowUnterminatedTrickleReader {
        delay: Duration,
    }
    impl Read for SlowUnterminatedTrickleReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(self.delay);
            buf[0] = b'x';
            Ok(1)
        }
    }

    #[test]
    fn drive_remains_cancellable_against_a_slow_unterminated_single_byte_trickle() {
        // Real cancellation, from another thread, exactly as a live turn's
        // cancel button would trigger it — not a pre-set flag `drive` only
        // has to notice once. Proves prompt termination (well under a
        // second) against a peer that would otherwise take on the order of
        // `MAX_STREAMED_TEXT_BYTES * delay` to ever hit the byte cap.
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = cancel.clone();
        let handle = std::thread::spawn(move || {
            let mut r = SseReader::new(SlowUnterminatedTrickleReader { delay: Duration::from_millis(20) });
            drive(&mut r, &cancel_for_thread, |_ev| DriveControl::Continue)
        });
        std::thread::sleep(Duration::from_millis(150));
        cancel.store(true, Ordering::Release);
        let outcome = handle.join().unwrap();
        assert!(
            matches!(outcome, DriveOutcome::Cancelled),
            "cancellation must not be starved by a slow, unterminated, never-idle byte trickle"
        );
    }

    /// A `Read` that endlessly yields well-formed, terminated `data:` lines
    /// and NEVER sends the blank-line terminator that would close the
    /// record — proves the per-record line-count cap fires.
    struct EndlessDataLinesReader;
    impl Read for EndlessDataLinesReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let line = b"data: x\n";
            let n = line.len().min(buf.len());
            buf[..n].copy_from_slice(&line[..n]);
            Ok(n)
        }
    }

    #[test]
    fn next_step_bails_out_of_an_unterminated_record_instead_of_looping_forever() {
        // A tiny line cap (not the real 100_000) so this test runs
        // instantly while still exercising the exact same bounded-cap
        // code path the production constant protects. Each call now
        // processes exactly one line (Progress) — next_event loops past
        // those until the cap trips.
        let mut r = SseReader::with_limits(EndlessDataLinesReader, MAX_STREAMED_TEXT_BYTES, 10);
        let err = next_event(&mut r).expect_err("an endless unterminated record must error, not hang");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// A `Read` that never emits a single `\n` at all — the 07d-A5
    /// fix-verification review's finding: the byte cap must be checked
    /// incrementally per underlying read, not only once a full line is
    /// assembled, or a single endless unterminated line could grow forever
    /// inside one `next_step()` call regardless of any per-line cap.
    struct EndlessNoNewlineReader;
    impl Read for EndlessNoNewlineReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            // Capped at a fixed chunk size (not `buf.len()`) so this reader
            // forces several `fill_buf` iterations before the byte cap
            // trips, proving genuine incremental checking rather than
            // happening to fit in one call.
            let n = buf.len().min(4096);
            for b in buf[..n].iter_mut() {
                *b = b'x';
            }
            Ok(n)
        }
    }

    #[test]
    fn next_step_bails_out_of_a_single_endless_unterminated_line_via_incremental_checks() {
        // Each call processes one fill_buf's worth (4096 bytes) and returns
        // Progress — next_event loops past those (three calls: 4096, 8192,
        // then 12288 > 10_000 trips the cap), proving genuine incremental
        // checking across separate returns, not just within one call.
        let mut r = SseReader::with_limits(EndlessNoNewlineReader, 10_000, MAX_LINES_PER_RECORD);
        let err = next_event(&mut r).expect_err("an endless unterminated line must error, not hang or grow unbounded");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn next_step_bails_out_once_a_single_record_exceeds_the_byte_cap() {
        // Neither line is blank, so each is its own Progress step — the
        // first (17 bytes) stays under the 20-byte cap, the second (34
        // bytes cumulative) trips it. next_event loops past the first.
        let mut chunks = std::collections::VecDeque::new();
        chunks.push_back(Ok(b"data: 0123456789\n".to_vec())); // 17 bytes
        chunks.push_back(Ok(b"data: 0123456789\n".to_vec())); // another 17 bytes, no terminator
        let mut r = SseReader::with_limits(ScriptedReader { chunks }, 20, MAX_LINES_PER_RECORD);
        let err = next_event(&mut r).expect_err("cumulative bytes across both lines exceed the cap");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// A blank-keepalive flood (no data:/event: content at all, so the
    /// byte cap on payload content never trips) must still be bounded by
    /// the independent line-count cap.
    struct EndlessBlankLinesReader;
    impl Read for EndlessBlankLinesReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let line = b"\n";
            let n = line.len().min(buf.len());
            buf[..n].copy_from_slice(&line[..n]);
            Ok(n)
        }
    }

    #[test]
    fn next_step_bails_out_of_an_endless_blank_keepalive_flood() {
        let mut r = SseReader::with_limits(EndlessBlankLinesReader, MAX_STREAMED_TEXT_BYTES, 10);
        let err = next_event(&mut r).expect_err("an endless blank-line flood must error, not hang");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}

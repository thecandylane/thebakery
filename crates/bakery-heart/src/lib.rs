//! bakery-heart — SQLite-backed event store for the Knowledge Heart (M1).
//!
//! Implements the mutation log only: per-stream, hash-chained, append-only.
//! The access log (read audit) is deferred (KNOWLEDGE_HEART §3.3).
//!
//! Ordering is PER STREAM via `(stream_id, seq)`; `prev_hash` chains within a
//! stream. Different streams are independent, so distinct worktrees can be
//! written concurrently with no global serialization point.

use bakery_core::{verify_chain, ChainError, Event, NewEvent};
use rusqlite::{params, Connection, TransactionBehavior};

/// Errors from the store.
#[derive(Debug)]
pub enum HeartError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Chain(ChainError),
}

impl std::fmt::Display for HeartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeartError::Sqlite(e) => write!(f, "sqlite error: {e}"),
            HeartError::Json(e) => write!(f, "json error: {e}"),
            HeartError::Chain(e) => write!(f, "chain error: {e}"),
        }
    }
}

impl std::error::Error for HeartError {}

impl From<rusqlite::Error> for HeartError {
    fn from(e: rusqlite::Error) -> Self {
        HeartError::Sqlite(e)
    }
}
impl From<serde_json::Error> for HeartError {
    fn from(e: serde_json::Error) -> Self {
        HeartError::Json(e)
    }
}
impl From<ChainError> for HeartError {
    fn from(e: ChainError) -> Self {
        HeartError::Chain(e)
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    stream_id    TEXT    NOT NULL,
    seq          INTEGER NOT NULL,
    content_hash TEXT    NOT NULL,
    prev_hash    TEXT,
    actor        TEXT    NOT NULL,
    ts           TEXT    NOT NULL,
    causality    TEXT    NOT NULL,
    layer        TEXT,
    sensitivity  TEXT    NOT NULL DEFAULT 'normal',
    family       TEXT    NOT NULL,
    payload      TEXT    NOT NULL,
    event_hash   TEXT    NOT NULL,
    PRIMARY KEY (stream_id, seq)
);
";

/// The mutation-log store.
pub struct Heart {
    conn: Connection,
    /// Called with the committed event's family after a successful append.
    /// Runs on the append thread. The caller may already hold the mutex
    /// around this `Heart`; the hook must not lock it.
    on_append: Option<std::sync::Arc<dyn Fn(&str) + Send + Sync>>,
}

impl Heart {
    /// Open (creating if needed) a file-backed store with WAL enabled.
    pub fn open(path: &str) -> Result<Self, HeartError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            on_append: None,
        })
    }

    /// Open an ephemeral in-memory store (tests).
    pub fn open_in_memory() -> Result<Self, HeartError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            on_append: None,
        })
    }

    /// Install the hook called with each committed event's family.
    /// The hook runs on the append thread and must not lock this `Heart`.
    pub fn set_on_append(&mut self, hook: impl Fn(&str) + Send + Sync + 'static) {
        self.on_append = Some(std::sync::Arc::new(hook));
    }

    /// Append a new event to its stream. Reads the stream tip and inserts under a
    /// single IMMEDIATE transaction so `seq`/`prev_hash` assignment cannot race
    /// with a concurrent append to the same stream. Returns the sealed event.
    pub fn append(&mut self, new: NewEvent) -> Result<Event, HeartError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        // Current tip of this stream, if any.
        let tip: Option<(u64, String)> = match tx.query_row(
            "SELECT seq, event_hash FROM events WHERE stream_id = ?1 ORDER BY seq DESC LIMIT 1",
            params![new.stream_id],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?)),
        ) {
            Ok(v) => Some(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };

        let (seq, prev_hash) = match tip {
            Some((s, h)) => (s + 1, Some(h)),
            None => (0, None),
        };

        let event = new.seal(seq, prev_hash);
        let causality_json = serde_json::to_string(&event.causality)?;

        tx.execute(
            "INSERT INTO events
                (stream_id, seq, content_hash, prev_hash, actor, ts,
                 causality, layer, sensitivity, family, payload, event_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                event.stream_id,
                event.seq as i64,
                event.content_hash,
                event.prev_hash,
                event.actor,
                event.ts,
                causality_json,
                event.layer,
                event.sensitivity,
                event.family,
                event.payload,
                event.event_hash,
            ],
        )?;

        tx.commit()?;
        if let Some(hook) = self.on_append.clone() {
            hook(&event.family);
        }
        Ok(event)
    }

    /// Read a full stream in ascending `seq` order.
    pub fn read_stream(&self, stream_id: &str) -> Result<Vec<Event>, HeartError> {
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, seq, content_hash, prev_hash, actor, ts,
                    causality, layer, sensitivity, family, payload, event_hash
             FROM events WHERE stream_id = ?1 ORDER BY seq ASC",
        )?;

        // Pull raw rows first (causality is JSON text parsed outside the SQL closure,
        // since serde errors are not rusqlite errors).
        let raw: Vec<RawRow> = stmt
            .query_map(params![stream_id], RawRow::from_row)?
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;

        let mut out = Vec::with_capacity(raw.len());
        for row in raw {
            out.push(row.into_event()?);
        }
        Ok(out)
    }

    /// Read the last `limit` events of a stream in ascending `seq` order.
    /// Returns an empty vec when the stream does not exist or `limit` is 0.
    /// Order matches `read_stream`, so callers need no adaptation.
    pub fn read_stream_tail(
        &self,
        stream_id: &str,
        limit: usize,
    ) -> Result<Vec<Event>, HeartError> {
        if limit == 0 {
            return Ok(vec![]);
        }
        // SQLite treats negative LIMIT as unlimited; never cast a huge usize to i64 unchecked.
        let lim = i64::try_from(limit).unwrap_or(i64::MAX).max(0);
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, seq, content_hash, prev_hash, actor, ts,
                    causality, layer, sensitivity, family, payload, event_hash
             FROM events WHERE stream_id = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let raw: Vec<RawRow> = stmt
            .query_map(params![stream_id, lim], RawRow::from_row)?
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;
        // DESC query returns newest first; reverse to ascending seq for consumers.
        let mut out = Vec::with_capacity(raw.len());
        for row in raw.into_iter().rev() {
            out.push(row.into_event()?);
        }
        Ok(out)
    }

    /// Summarize every stream in the store, most recently active first.
    /// Cheap enough to call on every client connect (one grouped scan).
    pub fn list_streams(&self) -> Result<Vec<StreamSummary>, HeartError> {
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, COUNT(*), MIN(ts), MAX(ts)
             FROM events GROUP BY stream_id ORDER BY MAX(ts) DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(StreamSummary {
                    stream_id: r.get(0)?,
                    events: r.get::<_, i64>(1)? as u64,
                    first_ts: r.get(2)?,
                    last_ts: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;
        Ok(rows)
    }

    /// Every event of one family in a stream, ascending `seq` order. For a
    /// family whose payload carries its own key (e.g. `seat.model.assigned`,
    /// where one stream has one such event per seat) — `first_of_family` /
    /// `last_of_family` only ever answer for the family as a whole, so a
    /// caller that needs "the latest event for key K within this family"
    /// reads them all and filters by payload itself.
    pub fn events_of_family(
        &self,
        stream_id: &str,
        family: &str,
    ) -> Result<Vec<Event>, HeartError> {
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, seq, content_hash, prev_hash, actor, ts,
                    causality, layer, sensitivity, family, payload, event_hash
             FROM events WHERE stream_id = ?1 AND family = ?2 ORDER BY seq ASC",
        )?;
        let raw: Vec<RawRow> = stmt
            .query_map(params![stream_id, family], RawRow::from_row)?
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;
        let mut out = Vec::with_capacity(raw.len());
        for row in raw {
            out.push(row.into_event()?);
        }
        Ok(out)
    }

    /// First event of a family in a stream (e.g. the `project.onboarded` record).
    pub fn first_of_family(
        &self,
        stream_id: &str,
        family: &str,
    ) -> Result<Option<Event>, HeartError> {
        self.one_of_family(stream_id, family, "ASC")
    }

    /// Most recent event of a family in a stream (e.g. the current ownership claim).
    pub fn last_of_family(
        &self,
        stream_id: &str,
        family: &str,
    ) -> Result<Option<Event>, HeartError> {
        self.one_of_family(stream_id, family, "DESC")
    }

    /// The newest event in `stream_id` whose family is any of `families` —
    /// one backward scan, however many families are asked about.
    pub fn last_of_families(
        &self,
        stream_id: &str,
        families: &[&str],
    ) -> Result<Option<Event>, HeartError> {
        if families.is_empty() {
            return Ok(None);
        }
        // Only placeholders are formatted in; every value is bound.
        let marks = vec!["?"; families.len()].join(", ");
        let sql = format!(
            "SELECT stream_id, seq, content_hash, prev_hash, actor, ts,
                    causality, layer, sensitivity, family, payload, event_hash
             FROM events WHERE stream_id = ?1 AND family IN ({marks}) ORDER BY seq DESC LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let values = std::iter::once(stream_id).chain(families.iter().copied());
        match stmt.query_row(rusqlite::params_from_iter(values), RawRow::from_row) {
            Ok(row) => Ok(Some(row.into_event()?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// At most `limit` events of any of `families` in one stream, oldest
    /// first: the first ones after `after_seq`, or the newest ones when it is
    /// `None`.
    pub fn events_of_families(
        &self,
        stream_id: &str,
        families: &[&str],
        after_seq: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Event>, HeartError> {
        if families.is_empty() || limit == 0 {
            return Ok(vec![]);
        }
        // Only placeholders are formatted in; every value is bound.
        let marks = vec!["?"; families.len()].join(", ");
        let (after, order) = match after_seq {
            Some(seq) => (i64::try_from(seq).unwrap_or(i64::MAX), "ASC"),
            None => (-1, "DESC"),
        };
        let sql = format!(
            "SELECT stream_id, seq, content_hash, prev_hash, actor, ts,
                    causality, layer, sensitivity, family, payload, event_hash
             FROM events WHERE stream_id = ? AND seq > ? AND family IN ({marks})
             ORDER BY seq {order} LIMIT ?"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let values = [
            rusqlite::types::Value::Text(stream_id.to_string()),
            rusqlite::types::Value::Integer(after),
        ]
        .into_iter()
        .chain(families.iter().map(|f| rusqlite::types::Value::Text(f.to_string())))
        .chain([rusqlite::types::Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX))]);
        let mut raw = stmt
            .query_map(rusqlite::params_from_iter(values), RawRow::from_row)?
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;
        if after_seq.is_none() {
            raw.reverse();
        }
        raw.into_iter().map(RawRow::into_event).collect()
    }

    /// Events of any of `families` appended after `position` (0: from the
    /// start), oldest first, each with its own position: the row's SQLite
    /// rowid, which only grows, since the log is only ever appended to. For
    /// an index derived from the log that keeps the position it read up to.
    pub fn events_after(
        &self,
        position: i64,
        families: &[&str],
    ) -> Result<Vec<(i64, Event)>, HeartError> {
        if families.is_empty() {
            return Ok(vec![]);
        }
        // Only placeholders are formatted in; every value is bound.
        let marks = vec!["?"; families.len()].join(", ");
        let sql = format!(
            "SELECT stream_id, seq, content_hash, prev_hash, actor, ts,
                    causality, layer, sensitivity, family, payload, event_hash, rowid
             FROM events WHERE rowid > ? AND family IN ({marks}) ORDER BY rowid ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let values = std::iter::once(rusqlite::types::Value::Integer(position))
            .chain(families.iter().map(|f| rusqlite::types::Value::Text(f.to_string())));
        let raw = stmt
            .query_map(rusqlite::params_from_iter(values), |r| {
                Ok((r.get::<_, i64>(12)?, RawRow::from_row(r)?))
            })?
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;
        raw.into_iter()
            .map(|(at, row)| Ok((at, row.into_event()?)))
            .collect()
    }

    /// The event with this hash, wherever it is: a full scan, for a rare
    /// look-up (a note's evidence).
    pub fn find(&self, event_hash: &str) -> Result<Option<Event>, HeartError> {
        let row = self.conn.query_row(
            "SELECT stream_id, seq, content_hash, prev_hash, actor, ts,
                    causality, layer, sensitivity, family, payload, event_hash
             FROM events WHERE event_hash = ?1 LIMIT 1",
            params![event_hash],
            RawRow::from_row,
        );
        match row {
            Ok(row) => Ok(Some(row.into_event()?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The newest event's position and hash; none for an empty log.
    pub fn tip(&self) -> Result<Option<(i64, String)>, HeartError> {
        let tip = self.conn.query_row(
            "SELECT rowid, event_hash FROM events ORDER BY rowid DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        );
        match tip {
            Ok(tip) => Ok(Some(tip)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The hash of the event at `position`, if there is one: whether a
    /// position read earlier is still the same event (a log started over
    /// has others there).
    pub fn hash_at(&self, position: i64) -> Result<Option<String>, HeartError> {
        let hash = self.conn.query_row(
            "SELECT event_hash FROM events WHERE rowid = ?1",
            params![position],
            |r| r.get(0),
        );
        match hash {
            Ok(hash) => Ok(Some(hash)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn one_of_family(
        &self,
        stream_id: &str,
        family: &str,
        dir: &str,
    ) -> Result<Option<Event>, HeartError> {
        // `dir` is a literal from this module only — never user input.
        let sql = format!(
            "SELECT stream_id, seq, content_hash, prev_hash, actor, ts,
                    causality, layer, sensitivity, family, payload, event_hash
             FROM events WHERE stream_id = ?1 AND family = ?2 ORDER BY seq {dir} LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let raw = stmt.query_row(params![stream_id, family], RawRow::from_row);
        match raw {
            Ok(row) => Ok(Some(row.into_event()?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Read a stream and verify its hash chain end to end.
    pub fn verify_stream(&self, stream_id: &str) -> Result<(), HeartError> {
        let events = self.read_stream(stream_id)?;
        verify_chain(&events)?;
        Ok(())
    }

    /// Escape hatch for tests that need to corrupt storage directly.
    #[cfg(test)]
    fn conn(&self) -> &Connection {
        &self.conn
    }
}

/// One stream's shape at a glance — what "recents" is built from.
#[derive(Debug, Clone)]
pub struct StreamSummary {
    pub stream_id: String,
    pub events: u64,
    pub first_ts: String,
    pub last_ts: String,
}

struct RawRow {
    stream_id: String,
    seq: u64,
    content_hash: String,
    prev_hash: Option<String>,
    actor: String,
    ts: String,
    causality: String,
    layer: Option<String>,
    sensitivity: String,
    family: String,
    payload: String,
    event_hash: String,
}

impl RawRow {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            stream_id: r.get(0)?,
            seq: r.get::<_, i64>(1)? as u64,
            content_hash: r.get(2)?,
            prev_hash: r.get(3)?,
            actor: r.get(4)?,
            ts: r.get(5)?,
            causality: r.get(6)?,
            layer: r.get(7)?,
            sensitivity: r.get(8)?,
            family: r.get(9)?,
            payload: r.get(10)?,
            event_hash: r.get(11)?,
        })
    }

    /// Parse the JSON-encoded causality column outside the rusqlite closure
    /// (serde errors are not rusqlite errors).
    fn into_event(self) -> Result<Event, HeartError> {
        Ok(Event {
            stream_id: self.stream_id,
            seq: self.seq,
            content_hash: self.content_hash,
            prev_hash: self.prev_hash,
            actor: self.actor,
            ts: self.ts,
            causality: serde_json::from_str(&self.causality)?,
            layer: self.layer,
            sensitivity: self.sensitivity,
            family: self.family,
            payload: self.payload,
            event_hash: self.event_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_event(stream: &str, payload: &str) -> NewEvent {
        NewEvent {
            stream_id: stream.to_string(),
            actor: "human".to_string(),
            ts: "2026-07-23T00:00:00Z".to_string(),
            causality: vec![],
            layer: Some("L2".to_string()),
            sensitivity: "normal".to_string(),
            family: "test.event".to_string(),
            payload: payload.to_string(),
        }
    }

    #[test]
    fn append_assigns_contiguous_seq_and_links() {
        let mut h = Heart::open_in_memory().unwrap();
        let e0 = h.append(new_event("s", "a")).unwrap();
        let e1 = h.append(new_event("s", "b")).unwrap();
        assert_eq!(e0.seq, 0);
        assert_eq!(e1.seq, 1);
        assert_eq!(e0.prev_hash, None);
        assert_eq!(e1.prev_hash.as_deref(), Some(e0.event_hash.as_str()));
    }

    #[test]
    fn read_stream_roundtrips_and_verifies() {
        let mut h = Heart::open_in_memory().unwrap();
        for p in ["a", "b", "c"] {
            h.append(new_event("s", p)).unwrap();
        }
        let got = h.read_stream("s").unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(h.verify_stream("s").is_ok());
    }

    #[test]
    fn streams_are_independent() {
        let mut h = Heart::open_in_memory().unwrap();
        h.append(new_event("a", "a0")).unwrap();
        h.append(new_event("b", "b0")).unwrap();
        h.append(new_event("a", "a1")).unwrap();
        assert_eq!(h.read_stream("a").unwrap().len(), 2);
        assert_eq!(h.read_stream("b").unwrap().len(), 1);
        // Each stream restarts seq from 0.
        assert_eq!(h.read_stream("b").unwrap()[0].seq, 0);
        assert!(h.verify_stream("a").is_ok());
        assert!(h.verify_stream("b").is_ok());
    }

    #[test]
    fn events_of_families_pages_one_stream_by_family() {
        let mut h = Heart::open_in_memory().unwrap();
        for (stream, family) in [("s", "a"), ("s", "b"), ("t", "a"), ("s", "a"), ("s", "c"), ("s", "a")] {
            let mut e = new_event(stream, family);
            e.family = family.to_string();
            h.append(e).unwrap();
        }
        let seqs = |after, limit| {
            h.events_of_families("s", &["a", "b"], after, limit)
                .unwrap()
                .iter()
                .map(|e| e.seq)
                .collect::<Vec<_>>()
        };
        assert_eq!(seqs(None, 2), vec![2, 4], "the newest, oldest first");
        assert_eq!(seqs(Some(0), 2), vec![1, 2], "the first after a seq");
        assert_eq!(seqs(Some(4), 10), Vec::<u64>::new());
        assert!(h.events_of_families("s", &[], None, 5).unwrap().is_empty());
    }

    #[test]
    fn payload_tamper_detected() {
        let mut h = Heart::open_in_memory().unwrap();
        for p in ["a", "b", "c"] {
            h.append(new_event("s", p)).unwrap();
        }
        h.conn()
            .execute(
                "UPDATE events SET payload = 'evil' WHERE stream_id = 's' AND seq = 1",
                [],
            )
            .unwrap();
        match h.verify_stream("s") {
            Err(HeartError::Chain(ChainError::ContentHashMismatch { index })) => {
                assert_eq!(index, 1)
            }
            other => panic!("expected ContentHashMismatch at 1, got {other:?}"),
        }
    }

    #[test]
    fn metadata_tamper_detected() {
        let mut h = Heart::open_in_memory().unwrap();
        for p in ["a", "b"] {
            h.append(new_event("s", p)).unwrap();
        }
        // Rewrite actor only; payload untouched → content_hash still matches,
        // only event_hash catches it.
        h.conn()
            .execute(
                "UPDATE events SET actor = 'attacker' WHERE stream_id = 's' AND seq = 0",
                [],
            )
            .unwrap();
        match h.verify_stream("s") {
            Err(HeartError::Chain(ChainError::EventHashMismatch { index })) => assert_eq!(index, 0),
            other => panic!("expected EventHashMismatch at 0, got {other:?}"),
        }
    }

    fn family_event(stream: &str, family: &str, ts: &str) -> NewEvent {
        NewEvent {
            stream_id: stream.to_string(),
            actor: "human".to_string(),
            ts: ts.to_string(),
            causality: vec![],
            layer: Some("L2".to_string()),
            sensitivity: "normal".to_string(),
            family: family.to_string(),
            payload: "{}".to_string(),
        }
    }

    #[test]
    fn list_streams_summarizes_newest_activity_first() {
        let mut h = Heart::open_in_memory().unwrap();
        h.append(family_event(
            "old",
            "project.onboarded",
            "2026-01-01T00:00:00Z",
        ))
        .unwrap();
        h.append(family_event(
            "new",
            "project.onboarded",
            "2026-05-01T00:00:00Z",
        ))
        .unwrap();
        h.append(family_event(
            "new",
            "agent.file.changed",
            "2026-06-01T00:00:00Z",
        ))
        .unwrap();

        let s = h.list_streams().unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(
            s[0].stream_id, "new",
            "most recently active stream sorts first"
        );
        assert_eq!(s[0].events, 2);
        assert_eq!(s[0].first_ts, "2026-05-01T00:00:00Z");
        assert_eq!(s[0].last_ts, "2026-06-01T00:00:00Z");
        assert_eq!(s[1].stream_id, "old");
    }

    #[test]
    fn first_and_last_of_family_pick_the_right_ends() {
        let mut h = Heart::open_in_memory().unwrap();
        h.append(family_event("s", "project.onboarded", "t0"))
            .unwrap();
        h.append(family_event("s", "stream.ownership.claimed", "t1"))
            .unwrap();
        h.append(family_event("s", "agent.file.changed", "t2"))
            .unwrap();
        h.append(family_event("s", "stream.ownership.claimed", "t3"))
            .unwrap();

        assert_eq!(
            h.first_of_family("s", "stream.ownership.claimed")
                .unwrap()
                .unwrap()
                .seq,
            1
        );
        assert_eq!(
            h.last_of_family("s", "stream.ownership.claimed")
                .unwrap()
                .unwrap()
                .seq,
            3
        );
        assert!(h.first_of_family("s", "nope.absent").unwrap().is_none());
        assert!(h
            .last_of_family("absent", "project.onboarded")
            .unwrap()
            .is_none());

        let newest = |families: &[&str]| h.last_of_families("s", families).unwrap().map(|e| e.seq);
        assert_eq!(
            newest(&["project.onboarded", "agent.file.changed"]),
            Some(2)
        );
        assert_eq!(
            newest(&["agent.file.changed", "stream.ownership.claimed"]),
            Some(3)
        );
        assert_eq!(newest(&["nope.absent"]), None);
        assert_eq!(newest(&[]), None);
        assert!(h
            .last_of_families("absent", &["project.onboarded"])
            .unwrap()
            .is_none());
    }

    #[test]
    fn events_of_family_returns_every_match_in_seq_order() {
        let mut h = Heart::open_in_memory().unwrap();
        h.append(family_event("s", "project.onboarded", "t0"))
            .unwrap();
        h.append(family_event("s", "stream.ownership.claimed", "t1"))
            .unwrap();
        h.append(family_event("s", "agent.file.changed", "t2"))
            .unwrap();
        h.append(family_event("s", "stream.ownership.claimed", "t3"))
            .unwrap();

        let got = h.events_of_family("s", "stream.ownership.claimed").unwrap();
        assert_eq!(got.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 3]);
        assert!(h.events_of_family("s", "nope.absent").unwrap().is_empty());
        assert!(h
            .events_of_family("absent", "project.onboarded")
            .unwrap()
            .is_empty());
    }

    /// An index reads the families it keeps from where it left off, across
    /// streams, and can tell when the log under that place is another one.
    #[test]
    fn events_after_reads_on_from_a_position_across_streams() {
        let mut h = Heart::open_in_memory().unwrap();
        assert_eq!(h.tip().unwrap(), None);
        h.append(family_event("a", "memory.written", "t0")).unwrap();
        h.append(family_event("b", "agent.turn.done", "t1")).unwrap();
        let second = h.append(family_event("b", "memory.written", "t2")).unwrap();

        let all = h.events_after(0, &["memory.written"]).unwrap();
        let streams: Vec<_> = all.iter().map(|(_, e)| e.stream_id.as_str()).collect();
        assert_eq!(streams, ["a", "b"]);
        let (tip, hash) = h.tip().unwrap().unwrap();
        assert_eq!((tip, hash.as_str()), (all[1].0, second.event_hash.as_str()));
        assert!(h.events_after(tip, &["memory.written"]).unwrap().is_empty());
        h.append(family_event("a", "memory.written", "t3")).unwrap();
        assert_eq!(h.events_after(tip, &["memory.written"]).unwrap().len(), 1);
        assert_eq!(h.hash_at(tip).unwrap(), Some(second.event_hash.clone()));
        assert_eq!(h.hash_at(tip + 10).unwrap(), None);
        assert!(h.events_after(0, &[]).unwrap().is_empty());
        let found = h.find(&second.event_hash).unwrap().unwrap();
        assert_eq!((found.stream_id.as_str(), found.seq), ("b", 1));
        assert!(h.find("no-such-hash").unwrap().is_none());
    }

    #[test]
    fn tail_of_missing_stream_returns_empty() {
        let h = Heart::open_in_memory().unwrap();
        assert_eq!(h.read_stream_tail("no-such-stream", 10).unwrap().len(), 0);
    }

    #[test]
    fn tail_with_limit_zero_returns_empty() {
        let mut h = Heart::open_in_memory().unwrap();
        h.append(new_event("s", "a")).unwrap();
        assert_eq!(h.read_stream_tail("s", 0).unwrap().len(), 0);
    }

    #[test]
    fn tail_fewer_than_limit_returns_all_ascending() {
        let mut h = Heart::open_in_memory().unwrap();
        for p in ["a", "b", "c"] {
            h.append(new_event("s", p)).unwrap();
        }
        let got = h.read_stream_tail("s", 10).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn tail_more_than_limit_returns_newest_ascending() {
        let mut h = Heart::open_in_memory().unwrap();
        for i in 0..10u64 {
            h.append(new_event("s", &i.to_string())).unwrap();
        }
        // ask for the last 3 — must be seqs 7, 8, 9 in ascending order
        let got = h.read_stream_tail("s", 3).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![7, 8, 9]);
    }

    #[test]
    fn duplicate_stream_seq_rejected_by_pk() {
        let mut h = Heart::open_in_memory().unwrap();
        h.append(new_event("s", "a")).unwrap();
        let dup = h.conn().execute(
            "INSERT INTO events (stream_id, seq, content_hash, actor, ts, causality, sensitivity, family, payload, event_hash)
             VALUES ('s', 0, 'x', 'a', 't', '[]', 'normal', 'f', 'p', 'e')",
            [],
        );
        assert!(dup.is_err(), "PK (stream_id, seq) must reject a duplicate");
    }
}

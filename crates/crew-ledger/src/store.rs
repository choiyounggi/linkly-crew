use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crew_bus::BusEvent;
use crew_proto::Envelope;

/// DESIGN.md §5/§8: append-only event ledger. No update/delete is exposed —
/// the API surface itself is the append-only guarantee.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialize error: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// A `messages` row, restored from `envelope_json` verbatim — contract §C2.
#[derive(Debug)]
pub struct StoredMessage {
    pub seq: i64,
    pub envelope: Envelope,
}

pub struct EventLedger {
    conn: Mutex<Connection>,
}

impl EventLedger {
    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, LedgerError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self, LedgerError> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload_json TEXT NOT NULL
            )",
            [],
        )?;
        // Contract §C2: structured, queryable mirror of accepted envelopes,
        // append-only like `events` (no UPDATE/DELETE exposed).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS messages (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT NOT NULL UNIQUE,
                ts TEXT NOT NULL,
                sprint TEXT NOT NULL,
                thread TEXT NOT NULL,
                from_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                corr TEXT NOT NULL,
                envelope_json TEXT NOT NULL
            )",
            [],
        )?;
        // Contract §E6: full-text index over `messages`, kept in sync from
        // the same `append` transaction. `msg_seq` is UNINDEXED (a join key,
        // not searched text).
        conn.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                msg_seq UNINDEXED, from_id, kind, text
            )",
            [],
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Appends one `BusEvent`, returning its assigned `seq`. `kind` is the
    /// event's variant name, taken from the externally-tagged JSON's single
    /// top-level key so it never drifts out of sync with `BusEvent`'s shape.
    ///
    /// `EnvelopeAccepted` additionally inserts into `messages` in the same
    /// transaction as the `events` insert (contract §C2), so a reader never
    /// observes one table updated without the other. Duplicate envelope
    /// `id`s (at-least-once redelivery) are ignored via `INSERT OR IGNORE`.
    pub fn append(&self, event: &BusEvent) -> Result<i64, LedgerError> {
        let payload = serde_json::to_value(event)?;
        let kind = payload
            .as_object()
            .and_then(|obj| obj.keys().next())
            .cloned()
            .expect("BusEvent's default externally-tagged Serialize always yields a single-key object");
        let payload_json = payload.to_string();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO events (ts, kind, payload_json) VALUES (?1, ?2, ?3)",
            params![ts, kind, payload_json],
        )?;
        let seq = tx.last_insert_rowid();

        if let BusEvent::EnvelopeAccepted { envelope } = event {
            let kind_wire = serde_json::to_value(&envelope.kind)?
                .as_str()
                .expect("MessageKind always serializes to a wire string")
                .to_string();
            let envelope_json = serde_json::to_string(envelope)?;
            tx.execute(
                "INSERT OR IGNORE INTO messages
                    (id, ts, sprint, thread, from_id, kind, corr, envelope_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    envelope.id,
                    envelope.ts,
                    envelope.sprint,
                    envelope.thread,
                    envelope.from,
                    kind_wire,
                    envelope.corr,
                    envelope_json,
                ],
            )?;
            // Only index into `messages_fts` when the insert above actually
            // landed a new row — a duplicate `id` (at-least-once redelivery)
            // is ignored by `INSERT OR IGNORE`, so `changes() == 0` and the
            // fts row must stay out too (contract §E6 idempotence).
            if tx.changes() > 0 {
                let msg_seq = tx.last_insert_rowid();
                let text = format!("{} {}", envelope.body, envelope.thread);
                tx.execute(
                    "INSERT INTO messages_fts (msg_seq, from_id, kind, text)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![msg_seq, envelope.from, kind_wire, text],
                )?;
            }
        }

        tx.commit()?;
        Ok(seq)
    }

    pub fn count(&self) -> Result<i64, LedgerError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .map_err(LedgerError::from)
    }

    /// Returns the `payload_json` of every row of the given `kind`, in
    /// insertion order — read-only, for verification/replay.
    pub fn events_of_kind(&self, kind: &str) -> Result<Vec<String>, LedgerError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![kind], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Messages with `seq` strictly greater than the given seq, ascending —
    /// contract §C2. Restores each envelope from `envelope_json` verbatim
    /// (no per-column reassembly) so the reconstruction can never drift out
    /// of sync with `Envelope`'s shape.
    pub fn messages_since(&self, seq: i64) -> Result<Vec<StoredMessage>, LedgerError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT seq, envelope_json FROM messages WHERE seq > ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![seq], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        Self::collect_messages(rows)
    }

    /// Messages in the given thread, seq ascending — contract §C2.
    pub fn messages_in_thread(&self, thread: &str) -> Result<Vec<StoredMessage>, LedgerError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT seq, envelope_json FROM messages WHERE thread = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![thread], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        Self::collect_messages(rows)
    }

    /// Full-text search over `messages` via `messages_fts` — contract §E6.
    /// Each whitespace-separated token in `query` is quote-escaped and
    /// wrapped in double quotes before being joined back with spaces, so
    /// FTS5 syntax characters in user input (`"`, `*`, `-`, `NEAR(`, ...)
    /// are always treated as literal text, never as query syntax. An empty
    /// or whitespace-only query returns an empty result without touching
    /// sqlite at all. Results are ordered by rank, capped at `limit`.
    pub fn search_messages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, LedgerError> {
        let match_query = query
            .split_whitespace()
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        if match_query.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT m.seq, m.envelope_json
             FROM messages_fts f
             JOIN messages m ON m.seq = f.msg_seq
             WHERE messages_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![match_query, limit as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        Self::collect_messages(rows)
    }

    fn collect_messages(
        rows: impl Iterator<Item = rusqlite::Result<(i64, String)>>,
    ) -> Result<Vec<StoredMessage>, LedgerError> {
        let mut out = Vec::new();
        for row in rows {
            let (seq, envelope_json) = row?;
            let envelope: Envelope = serde_json::from_str(&envelope_json)?;
            out.push(StoredMessage { seq, envelope });
        }
        Ok(out)
    }
}

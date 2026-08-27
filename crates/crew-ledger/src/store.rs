use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crew_bus::BusEvent;

/// DESIGN.md §5/§8: append-only event ledger. No update/delete is exposed —
/// the API surface itself is the append-only guarantee.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialize error: {0}")]
    Serialize(#[from] serde_json::Error),
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
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Appends one `BusEvent`, returning its assigned `seq`. `kind` is the
    /// event's variant name, taken from the externally-tagged JSON's single
    /// top-level key so it never drifts out of sync with `BusEvent`'s shape.
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

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events (ts, kind, payload_json) VALUES (?1, ?2, ?3)",
            params![ts, kind, payload_json],
        )?;
        Ok(conn.last_insert_rowid())
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
}

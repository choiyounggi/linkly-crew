//! crew-ledger — append-only SQLite ledger for the bus's `BusEvent`
//! broadcast stream. DESIGN.md §5/§8 M3 scope: the BusEvent stream only,
//! not the full runs/sprints/tasks schema (M4+).

mod store;
mod subscribe;

pub use store::{EventLedger, LedgerError};
pub use subscribe::spawn_subscriber;

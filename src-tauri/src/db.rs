//! Skema riwayat lokal dengan WAL dan migrasi versi.

use std::path::Path;

use rusqlite::Connection;

const SCHEMA_VERSION: i64 = 1;

pub fn open(path: &Path) -> Result<Connection, String> {
    let conn =
        Connection::open(path).map_err(|e| format!("Tidak dapat membuka database riwayat: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("Tidak dapat mengatur timeout database riwayat: {e}"))?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn migrate(conn: &Connection) -> Result<(), String> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| format!("Tidak dapat mengaktifkan WAL: {e}"))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| format!("Tidak dapat mengatur synchronous: {e}"))?;
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| format!("Tidak dapat membaca versi skema: {e}"))?;
    if version < SCHEMA_VERSION {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS history (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 output TEXT NOT NULL,
                 engine TEXT NOT NULL,
                 tier TEXT NOT NULL,
                 at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_history_at ON history(at DESC, id DESC);
             PRAGMA user_version = 1;
             COMMIT;",
        )
        .map_err(|e| format!("Migrasi riwayat gagal: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_creates_schema_and_version() {
        let conn = Connection::open_in_memory().expect("in-memory db");
        migrate(&conn).expect("migrate");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='history'",
                [],
                |row| row.get(0),
            )
            .expect("table check");
        assert_eq!(count, 1);
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = Connection::open_in_memory().expect("in-memory db");
        migrate(&conn).expect("first migrate");
        migrate(&conn).expect("second migrate");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
    }
}

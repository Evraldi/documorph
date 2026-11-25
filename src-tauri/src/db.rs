//! Skema riwayat lokal dengan WAL dan migrasi versi.

use std::path::Path;

use rusqlite::Connection;

const SCHEMA_VERSION: i64 = 2;

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
             COMMIT;",
        )
        .map_err(|e| format!("Migrasi riwayat gagal: {e}"))?;
        if !has_column(conn, "history", "warnings")? {
            conn.execute(
                "ALTER TABLE history ADD COLUMN warnings TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|e| format!("Migrasi kolom warning gagal: {e}"))?;
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|e| format!("Tidak dapat menyimpan versi skema: {e}"))?;
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let sql = format!("PRAGMA table_info({table})");
    let mut statement = conn
        .prepare(&sql)
        .map_err(|e| format!("Tidak dapat membaca skema: {e}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| format!("Tidak dapat membaca kolom: {e}"))?;
    for row in rows {
        if row.map_err(|e| format!("Tidak dapat membaca kolom: {e}"))? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Warning disimpan sebagai baris yang dipisahkan newline, bukan JSON, supaya
/// ketergantungan `serde_json` tidak dipakai hanya untuk satu kolom.
pub fn encode_warnings(warnings: &[String]) -> String {
    warnings.join("\n")
}

pub fn decode_warnings(value: &str) -> Vec<String> {
    value
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
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
        assert!(has_column(&conn, "history", "warnings").expect("column"));
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

    #[test]
    fn legacy_schema_gains_warnings_column() {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            "CREATE TABLE history (id INTEGER PRIMARY KEY, name TEXT NOT NULL,
             output TEXT NOT NULL, engine TEXT NOT NULL, tier TEXT NOT NULL,
             at INTEGER NOT NULL); PRAGMA user_version = 1;",
        )
        .expect("legacy schema");
        migrate(&conn).expect("migrate legacy");
        assert!(has_column(&conn, "history", "warnings").expect("column"));
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn warnings_round_trip() {
        let warnings = vec!["satu".into(), "dua".into()];
        assert_eq!(decode_warnings(&encode_warnings(&warnings)), warnings);
    }
}

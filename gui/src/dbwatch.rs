//! Cheap "did the database change?" check for the file browser.

use clouddrop::config;
use rusqlite::{Connection, OpenFlags};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;

/// Watches SQLite's `PRAGMA data_version`, which changes whenever any other connection
/// (the daemon, the CLI, our own actions) commits.
pub struct DbWatch {
    path: PathBuf,
    conn: RefCell<Option<Connection>>,
    last: Cell<Option<i64>>,
}

impl DbWatch {
    pub fn new() -> Self {
        Self::at(config::data_dir().join("state.db"))
    }

    pub fn at(path: PathBuf) -> Self {
        DbWatch { path, conn: RefCell::default(), last: Cell::new(None) }
    }

    fn version(&self) -> Option<i64> {
        let mut conn = self.conn.borrow_mut();
        if conn.is_none() {
            *conn = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok();
        }
        let v = conn.as_ref()?.query_row("PRAGMA data_version", [], |r| r.get(0));
        match v {
            Ok(v) => Some(v),
            Err(_) => {
                *conn = None; // reopen next time (the file may have been replaced)
                None
            }
        }
    }

    /// True the first time, and whenever the database changed since the last call.
    /// If the database can't be read, reports a change so callers fall back to reloading.
    pub fn changed(&self) -> bool {
        let now = self.version();
        let was = self.last.replace(now);
        now.is_none() || now != was
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_commits_from_other_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer.execute_batch("CREATE TABLE t (x INTEGER);").unwrap();

        let watch = DbWatch::at(path);
        assert!(watch.changed(), "first call always reports a change");
        assert!(!watch.changed());
        assert!(!watch.changed());
        writer.execute("INSERT INTO t VALUES (1)", []).unwrap();
        assert!(watch.changed());
        assert!(!watch.changed());
    }

    #[test]
    fn missing_database_counts_as_changed() {
        let dir = tempfile::tempdir().unwrap();
        let watch = DbWatch::at(dir.path().join("nope.db"));
        assert!(watch.changed());
        assert!(watch.changed());
    }
}

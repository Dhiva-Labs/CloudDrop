use crate::util::{LocalMeta, now_ms};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Pending,
    Uploading,
    Synced,
    CloudOnly,
    Error,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "dir",
        }
    }
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Uploading => "uploading",
            State::Synced => "synced",
            State::CloudOnly => "cloud_only",
            State::Error => "error",
        }
    }

    fn parse(s: &str) -> rusqlite::Result<Self> {
        Ok(match s {
            "pending" => State::Pending,
            "uploading" => State::Uploading,
            "synced" => State::Synced,
            "cloud_only" => State::CloudOnly,
            "error" => State::Error,
            other => {
                return Err(rusqlite::Error::InvalidColumnType(
                    0,
                    format!("state {other}"),
                    rusqlite::types::Type::Text,
                ));
            }
        })
    }
}

/// One tracked file or directory. See docs/DESIGN.md for column meanings.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    pub state: State,
    pub drive_id: Option<String>,
    pub cloud_name: Option<String>,
    pub cloud_parent: Option<String>,
    pub md5: Option<String>,
    pub cloud_size: Option<i64>,
    pub local_size: Option<i64>,
    pub local_mtime_ns: Option<i64>,
    pub inode: Option<i64>,
    pub upload_url: Option<String>,
    pub upload_md5: Option<String>,
    pub pin_prev: bool,
    pub move_pending: bool,
    pub attempts: i64,
    pub retry_at: Option<i64>,
    pub error: Option<String>,
    /// Unix ms when the current Drive content was uploaded (drives automatic freeing).
    pub uploaded_at: Option<i64>,
}

impl Entry {
    pub fn new(path: &str, kind: Kind, meta: Option<LocalMeta>) -> Self {
        let mut e = Entry {
            path: path.to_string(),
            kind,
            state: State::Pending,
            drive_id: None,
            cloud_name: None,
            cloud_parent: None,
            md5: None,
            cloud_size: None,
            local_size: None,
            local_mtime_ns: None,
            inode: None,
            upload_url: None,
            upload_md5: None,
            pin_prev: false,
            move_pending: false,
            attempts: 0,
            retry_at: None,
            error: None,
            uploaded_at: None,
        };
        e.set_local(meta);
        e
    }

    pub fn local(&self) -> Option<LocalMeta> {
        Some(LocalMeta {
            size: self.local_size?,
            mtime_ns: self.local_mtime_ns?,
            inode: self.inode?,
        })
    }

    pub fn set_local(&mut self, meta: Option<LocalMeta>) {
        self.local_size = meta.map(|m| m.size);
        self.local_mtime_ns = meta.map(|m| m.mtime_ns);
        self.inode = meta.map(|m| m.inode);
    }

    /// Drive holds a usable copy: a folder, or a file with uploaded content.
    pub fn in_cloud(&self) -> bool {
        self.drive_id.is_some() && (self.kind == Kind::Dir || self.md5.is_some())
    }

    /// Queue for upload/placement now, clearing any failure backoff.
    pub fn requeue(&mut self) {
        self.state = State::Pending;
        self.attempts = 0;
        self.retry_at = None;
        self.error = None;
    }

    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let kind: String = r.get("kind")?;
        let state: String = r.get("state")?;
        Ok(Entry {
            path: r.get("path")?,
            kind: if kind == "dir" { Kind::Dir } else { Kind::File },
            state: State::parse(&state)?,
            drive_id: r.get("drive_id")?,
            cloud_name: r.get("cloud_name")?,
            cloud_parent: r.get("cloud_parent")?,
            md5: r.get("md5")?,
            cloud_size: r.get("cloud_size")?,
            local_size: r.get("local_size")?,
            local_mtime_ns: r.get("local_mtime_ns")?,
            inode: r.get("inode")?,
            upload_url: r.get("upload_url")?,
            upload_md5: r.get("upload_md5")?,
            pin_prev: r.get::<_, i64>("pin_prev")? != 0,
            move_pending: r.get::<_, i64>("move_pending")? != 0,
            attempts: r.get("attempts")?,
            retry_at: r.get("retry_at")?,
            error: r.get("error")?,
            uploaded_at: r.get("uploaded_at")?,
        })
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entries (
  path TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  state TEXT NOT NULL,
  drive_id TEXT,
  cloud_name TEXT,
  cloud_parent TEXT,
  md5 TEXT,
  cloud_size INTEGER,
  local_size INTEGER,
  local_mtime_ns INTEGER,
  inode INTEGER,
  upload_url TEXT,
  upload_md5 TEXT,
  pin_prev INTEGER NOT NULL DEFAULT 0,
  move_pending INTEGER NOT NULL DEFAULT 0,
  attempts INTEGER NOT NULL DEFAULT 0,
  retry_at INTEGER,
  error TEXT,
  updated_at INTEGER NOT NULL,
  uploaded_at INTEGER
);
CREATE INDEX IF NOT EXISTS entries_state ON entries(state);
CREATE INDEX IF NOT EXISTS entries_inode ON entries(inode);
CREATE INDEX IF NOT EXISTS entries_drive_id ON entries(drive_id);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS activity (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,
  kind TEXT NOT NULL,
  path TEXT NOT NULL,
  bytes INTEGER,
  detail TEXT
);
";

/// Activity rows kept; older ones are dropped.
const ACTIVITY_KEEP: i64 = 10_000;

/// Brings databases created by older versions up to date.
fn migrate(conn: &Connection) -> Result<()> {
    let has_uploaded_at = conn
        .prepare("SELECT 1 FROM pragma_table_info('entries') WHERE name = 'uploaded_at'")?
        .exists([])?;
    if !has_uploaded_at {
        conn.execute_batch(
            "ALTER TABLE entries ADD COLUMN uploaded_at INTEGER;
             UPDATE entries SET uploaded_at = updated_at WHERE md5 IS NOT NULL;",
        )?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS entries_uploaded_at ON entries(uploaded_at);")?;
    Ok(())
}

const COLS: &str = "path, kind, state, drive_id, cloud_name, cloud_parent, md5, cloud_size, \
    local_size, local_mtime_ns, inode, upload_url, upload_md5, pin_prev, move_pending, attempts, \
    retry_at, error, uploaded_at";

/// SQL condition selecting `prefix` and everything beneath it (params ?1 = prefix, ?2 = prefix/, ?3 = prefix0).
const SUBTREE: &str = "(path = ?1 OR (path >= ?2 AND path < ?3))";

fn subtree_params(prefix: &str) -> (String, String, String) {
    (
        prefix.to_string(),
        format!("{prefix}/"),
        format!("{prefix}0"),
    )
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        let db = Db {
            conn: Mutex::new(conn),
        };
        // Only write when it changes: every write bumps other connections' data_version.
        if db.meta_get("schema_version")?.as_deref() != Some("2") {
            db.meta_set("schema_version", "2")?;
        }
        Ok(db)
    }

    pub fn open_default() -> Result<Self> {
        Self::open(&crate::config::data_dir().join("state.db"))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Binds this database to a sync folder; refuses to mix two folders in one database.
    pub fn bind_root(&self, root: &Path) -> Result<()> {
        let root = root.to_string_lossy().to_string();
        match self.meta_get("sync_root")? {
            None => self.meta_set("sync_root", &root),
            Some(r) if r == root => Ok(()),
            Some(r) => bail!(
                "the state database belongs to {r}, but the config says {root}; \
                 move the folder back or start with a fresh CLOUDDROP_DATA_DIR"
            ),
        }
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn meta_delete(&self, key: &str) -> Result<()> {
        self.conn().execute("DELETE FROM meta WHERE key = ?1", [key])?;
        Ok(())
    }

    pub fn get(&self, path: &str) -> Result<Option<Entry>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLS} FROM entries WHERE path = ?1"),
                [path],
                Entry::from_row,
            )
            .optional()?)
    }

    pub fn put(&self, e: &Entry) -> Result<()> {
        put_with(&self.conn(), e)
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM entries WHERE path = ?1", [path])?;
        Ok(())
    }

    fn query(&self, sql: &str, p: impl rusqlite::Params) -> Result<Vec<Entry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(p, Entry::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `prefix` and all entries beneath it, parents before children. "" = everything.
    pub fn subtree(&self, prefix: &str) -> Result<Vec<Entry>> {
        if prefix.is_empty() {
            return self.query(&format!("SELECT {COLS} FROM entries ORDER BY path"), []);
        }
        let (a, b, c) = subtree_params(prefix);
        self.query(
            &format!("SELECT {COLS} FROM entries WHERE {SUBTREE} ORDER BY path"),
            params![a, b, c],
        )
    }

    /// Direct children of `dir` ("" = top level).
    pub fn children(&self, dir: &str) -> Result<Vec<Entry>> {
        let all = if dir.is_empty() {
            self.subtree("")?
        } else {
            let mut v = self.subtree(dir)?;
            v.retain(|e| e.path != dir);
            v
        };
        let skip = if dir.is_empty() { 0 } else { dir.len() + 1 };
        Ok(all
            .into_iter()
            .filter(|e| !e.path[skip..].contains('/'))
            .collect())
    }

    pub fn by_inode(&self, inode: i64) -> Result<Vec<Entry>> {
        self.query(
            &format!("SELECT {COLS} FROM entries WHERE inode = ?1"),
            [inode],
        )
    }

    /// Entries the worker should act on now, folders and shallow paths first.
    pub fn ready_jobs(&self, now: i64, limit: usize) -> Result<Vec<Entry>> {
        self.query(
            &format!(
                "SELECT {COLS} FROM entries \
                 WHERE (state = 'pending' OR (state = 'cloud_only' AND move_pending = 1)) \
                   AND (retry_at IS NULL OR retry_at <= ?1) \
                 ORDER BY kind = 'dir' DESC, length(path), path LIMIT ?2"
            ),
            params![now, limit as i64],
        )
    }

    /// Earliest future retry time among waiting entries, if any.
    pub fn next_retry(&self) -> Result<Option<i64>> {
        Ok(self.conn().query_row(
            "SELECT min(retry_at) FROM entries WHERE state = 'pending' AND retry_at IS NOT NULL",
            [],
            |r| r.get(0),
        )?)
    }

    /// Renames `from` and everything beneath it to `to`. Caller ensures `to` is free.
    pub fn move_subtree(&self, from: &str, to: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (a, b, c) = subtree_params(from);
        let n = tx.execute(
            &format!(
                "UPDATE entries SET path = ?4 || substr(path, ?5), updated_at = ?6 WHERE {SUBTREE}"
            ),
            params![a, b, c, to, from.len() as i64 + 1, now_ms()],
        )?;
        tx.commit()?;
        tracing::debug!("moved {n} entries {from} -> {to}");
        Ok(())
    }

    pub fn counts(&self) -> Result<Vec<(String, String, i64)>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT kind, state, count(*) FROM entries GROUP BY kind, state")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn cloud_bytes(&self) -> Result<(i64, i64)> {
        Ok(self.conn().query_row(
            "SELECT coalesce(sum(cloud_size), 0), \
                    coalesce(sum(CASE WHEN state = 'cloud_only' THEN cloud_size END), 0) \
             FROM entries WHERE kind = 'file' AND md5 IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Files waiting for or in upload.
    pub fn count_waiting(&self) -> Result<u64> {
        Ok(self.conn().query_row(
            "SELECT count(*) FROM entries WHERE kind = 'file' AND state IN ('pending', 'uploading')",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    pub fn errors(&self, limit: usize) -> Result<Vec<Entry>> {
        self.query(
            &format!("SELECT {COLS} FROM entries WHERE state = 'error' ORDER BY path LIMIT ?1"),
            [limit as i64],
        )
    }

    /// Crash recovery: anything marked uploading belongs to a dead process.
    pub fn reset_uploading(&self) -> Result<usize> {
        Ok(self.conn().execute(
            "UPDATE entries SET state = 'pending' WHERE state = 'uploading'",
            [],
        )?)
    }

    /// Re-queues failed entries under `prefix` ("" = all). Returns how many.
    pub fn retry(&self, prefix: &str) -> Result<usize> {
        let sql = "UPDATE entries SET state = 'pending', error = NULL, retry_at = NULL, \
                   attempts = 0, updated_at = ?4 WHERE state = 'error'";
        if prefix.is_empty() {
            return Ok(self.conn().execute(
                &sql.replace("?4", "?1"),
                params![now_ms()],
            )?);
        }
        let (a, b, c) = subtree_params(prefix);
        Ok(self.conn().execute(
            &format!("{sql} AND {SUBTREE}"),
            params![a, b, c, now_ms()],
        )?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    /// A new file reached Drive.
    Uploaded,
    /// A new version of an existing file reached Drive.
    Updated,
    /// Renamed or moved in Drive to follow a local rename/move.
    Moved,
    /// Removed locally; the Drive copy stays (now cloud-only).
    KeptInCloud,
    /// Local copy removed by Free Up Space.
    Freed,
    /// Local copy removed by automatic space saving.
    AutoFreed,
    /// Downloaded back from Drive.
    Restored,
    /// Upload failed permanently.
    Failed,
}

impl ActivityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActivityKind::Uploaded => "uploaded",
            ActivityKind::Updated => "updated",
            ActivityKind::Moved => "moved",
            ActivityKind::KeptInCloud => "kept_in_cloud",
            ActivityKind::Freed => "freed",
            ActivityKind::AutoFreed => "auto_freed",
            ActivityKind::Restored => "restored",
            ActivityKind::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "uploaded" => ActivityKind::Uploaded,
            "updated" => ActivityKind::Updated,
            "moved" => ActivityKind::Moved,
            "kept_in_cloud" => ActivityKind::KeptInCloud,
            "freed" => ActivityKind::Freed,
            "auto_freed" => ActivityKind::AutoFreed,
            "restored" => ActivityKind::Restored,
            "failed" => ActivityKind::Failed,
            _ => return None,
        })
    }
}

/// One line of history: what happened to which path, and when.
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    pub id: i64,
    /// Unix ms.
    pub at_ms: i64,
    pub kind: ActivityKind,
    pub path: String,
    pub bytes: Option<i64>,
    /// Extra text: the old path for moves, the error for failures.
    pub detail: Option<String>,
}

/// Totals over the files beneath a folder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FolderStats {
    pub files: i64,
    /// Size of all files, wherever they are.
    pub bytes: i64,
    /// Files present on this computer (uploaded or not).
    pub local: i64,
    pub cloud_only: i64,
    pub waiting: i64,
    pub failed: i64,
    /// Bytes kept only in Drive.
    pub cloud_only_bytes: i64,
}

/// A row for the file browser.
#[derive(Debug, Clone, PartialEq)]
pub struct DirItem {
    pub entry: Entry,
    /// Files: the file size. Folders: total size beneath.
    pub bytes: i64,
    /// Folders only.
    pub stats: Option<FolderStats>,
}

impl Entry {
    /// Best-known size: the Drive copy for cloud-only files, otherwise the local file.
    pub fn size(&self) -> i64 {
        match self.state {
            State::CloudOnly => self.cloud_size.unwrap_or(0),
            _ => self.local_size.or(self.cloud_size).unwrap_or(0),
        }
    }
}

impl Db {
    pub fn log_activity(
        &self,
        kind: ActivityKind,
        path: &str,
        bytes: Option<i64>,
        detail: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO activity (at, kind, path, bytes, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![now_ms(), kind.as_str(), path, bytes, detail],
        )?;
        let id = conn.last_insert_rowid();
        if id % 100 == 0 {
            conn.execute("DELETE FROM activity WHERE id <= ?1", [id - ACTIVITY_KEEP])?;
        }
        Ok(())
    }

    /// Newest first.
    pub fn recent_activity(&self, limit: usize) -> Result<Vec<Activity>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, at, kind, path, bytes, detail FROM activity ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, at_ms, kind, path, bytes, detail) = row?;
            if let Some(kind) = ActivityKind::parse(&kind) {
                out.push(Activity { id, at_ms, kind, path, bytes, detail });
            }
        }
        Ok(out)
    }

    /// Uploaded files whose upload and last local change are both at or before `cutoff_ms`.
    pub fn free_candidates(&self, cutoff_ms: i64, limit: usize) -> Result<Vec<Entry>> {
        self.query(
            &format!(
                "SELECT {COLS} FROM entries WHERE kind = 'file' AND state = 'synced' \
                   AND uploaded_at IS NOT NULL AND uploaded_at <= ?1 \
                   AND (local_mtime_ns IS NULL OR local_mtime_ns <= ?1 * 1000000) \
                 ORDER BY uploaded_at LIMIT ?2"
            ),
            params![cutoff_ms, limit as i64],
        )
    }

    /// Id of the newest activity row (0 if none) — cheap change detection.
    pub fn last_activity_id(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT coalesce(max(id), 0) FROM activity", [], |r| r.get(0))?)
    }

    /// Totals for the files beneath `dir` ("" = everything).
    pub fn folder_stats(&self, dir: &str) -> Result<FolderStats> {
        let (lo, hi) = if dir.is_empty() {
            (String::new(), "\u{10FFFF}".to_string())
        } else {
            (format!("{dir}/"), format!("{dir}0"))
        };
        Ok(self.conn().query_row(
            "SELECT count(*), \
               coalesce(sum(CASE WHEN state = 'cloud_only' THEN coalesce(cloud_size, 0) \
                                 ELSE coalesce(local_size, cloud_size, 0) END), 0), \
               count(*) FILTER (WHERE state <> 'cloud_only'), \
               count(*) FILTER (WHERE state = 'cloud_only'), \
               count(*) FILTER (WHERE state IN ('pending', 'uploading')), \
               count(*) FILTER (WHERE state = 'error'), \
               coalesce(sum(cloud_size) FILTER (WHERE state = 'cloud_only'), 0) \
             FROM entries WHERE kind = 'file' AND path >= ?1 AND path < ?2",
            params![lo, hi],
            |r| {
                Ok(FolderStats {
                    files: r.get(0)?,
                    bytes: r.get(1)?,
                    local: r.get(2)?,
                    cloud_only: r.get(3)?,
                    waiting: r.get(4)?,
                    failed: r.get(5)?,
                    cloud_only_bytes: r.get(6)?,
                })
            },
        )?)
    }

    fn dir_item(&self, e: Entry) -> Result<DirItem> {
        Ok(match e.kind {
            Kind::Dir => {
                let stats = self.folder_stats(&e.path)?;
                DirItem { bytes: stats.bytes, stats: Some(stats), entry: e }
            }
            Kind::File => DirItem { bytes: e.size(), stats: None, entry: e },
        })
    }

    /// Direct children of `dir` for the file browser, folders first then by name.
    pub fn list_dir(&self, dir: &str) -> Result<Vec<DirItem>> {
        let mut kids = self.children(dir)?;
        kids.sort_by(|a, b| {
            (a.kind != Kind::Dir, crate::util::name_of(&a.path).to_lowercase())
                .cmp(&(b.kind != Kind::Dir, crate::util::name_of(&b.path).to_lowercase()))
        });
        kids.into_iter().map(|e| self.dir_item(e)).collect()
    }

    /// Files and folders whose name contains `needle` (case-insensitive), anywhere.
    pub fn search(&self, needle: &str, limit: usize) -> Result<Vec<DirItem>> {
        let needle = needle.to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!(
            "%{}%",
            needle.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        );
        let hits = self.query(
            &format!(
                "SELECT {COLS} FROM entries WHERE lower(path) LIKE ?1 ESCAPE '\\' \
                 ORDER BY kind = 'dir' DESC, path LIMIT ?2"
            ),
            params![pattern, (limit * 4) as i64],
        )?;
        hits.into_iter()
            .filter(|e| crate::util::name_of(&e.path).to_lowercase().contains(&needle))
            .take(limit)
            .map(|e| self.dir_item(e))
            .collect()
    }
}

fn put_with(conn: &Connection, e: &Entry) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO entries ({COLS}, updated_at) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)"
        ),
        params![
            e.path,
            e.kind.as_str(),
            e.state.as_str(),
            e.drive_id,
            e.cloud_name,
            e.cloud_parent,
            e.md5,
            e.cloud_size,
            e.local_size,
            e.local_mtime_ns,
            e.inode,
            e.upload_url,
            e.upload_md5,
            e.pin_prev as i64,
            e.move_pending as i64,
            e.attempts,
            e.retry_at,
            e.error,
            e.uploaded_at,
            now_ms(),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("s.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn subtree_and_children_respect_boundaries() {
        let (_d, db) = db();
        for p in ["a", "a/x", "a/y/z", "ab", "a0", "b"] {
            db.put(&Entry::new(p, Kind::File, None)).unwrap();
        }
        let sub: Vec<_> = db.subtree("a").unwrap().into_iter().map(|e| e.path).collect();
        assert_eq!(sub, ["a", "a/x", "a/y/z"]);
        let top: Vec<_> = db.children("").unwrap().into_iter().map(|e| e.path).collect();
        assert_eq!(top, ["a", "a0", "ab", "b"]);
        let kids: Vec<_> = db.children("a").unwrap().into_iter().map(|e| e.path).collect();
        assert_eq!(kids, ["a/x"]);
    }

    #[test]
    fn move_subtree_rewrites_prefix_only() {
        let (_d, db) = db();
        for p in ["docs", "docs/a.txt", "docs/sub/b.txt", "docs2/c.txt"] {
            db.put(&Entry::new(p, Kind::File, None)).unwrap();
        }
        db.move_subtree("docs", "papers/docs").unwrap();
        let all: Vec<_> = db.subtree("").unwrap().into_iter().map(|e| e.path).collect();
        assert_eq!(
            all,
            ["docs2/c.txt", "papers/docs", "papers/docs/a.txt", "papers/docs/sub/b.txt"]
        );
    }

    #[test]
    fn activity_log_newest_first() {
        let (_d, db) = db();
        db.log_activity(ActivityKind::Uploaded, "a.txt", Some(3), None).unwrap();
        db.log_activity(ActivityKind::Moved, "b.txt", None, Some("a.txt")).unwrap();
        let acts = db.recent_activity(10).unwrap();
        assert_eq!(acts.len(), 2);
        assert_eq!(acts[0].kind, ActivityKind::Moved);
        assert_eq!(acts[0].detail.as_deref(), Some("a.txt"));
        assert_eq!(acts[1].bytes, Some(3));
        assert_eq!(db.last_activity_id().unwrap(), acts[0].id);
    }

    #[test]
    fn listing_and_folder_stats() {
        let (_d, db) = db();
        let file = |p: &str, state: State, size: i64| {
            let mut e = Entry::new(p, Kind::File, None);
            e.state = state;
            e.local_size = Some(size);
            e.cloud_size = Some(size);
            e
        };
        db.put(&Entry::new("Photos", Kind::Dir, None)).unwrap();
        db.put(&file("Photos/a.jpg", State::Synced, 10)).unwrap();
        db.put(&file("Photos/b.jpg", State::CloudOnly, 20)).unwrap();
        db.put(&file("Photos/c.jpg", State::Error, 5)).unwrap();
        db.put(&file("zeta.txt", State::Pending, 1)).unwrap();
        let top = db.list_dir("").unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].entry.path, "Photos");
        let st = top[0].stats.unwrap();
        assert_eq!((st.files, st.bytes, st.local, st.cloud_only, st.failed), (3, 35, 2, 1, 1));
        assert_eq!(st.cloud_only_bytes, 20);
        assert_eq!(top[1].bytes, 1);
        assert_eq!(db.folder_stats("").unwrap().files, 4);
        let hits: Vec<_> = db.search("JPG", 10).unwrap().into_iter().map(|i| i.entry.path).collect();
        assert_eq!(hits, ["Photos/a.jpg", "Photos/b.jpg", "Photos/c.jpg"]);
        assert!(db.search("photos/a", 10).unwrap().is_empty(), "matches names, not paths");
    }

    #[test]
    fn migrates_version_1_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE entries (path TEXT PRIMARY KEY, kind TEXT NOT NULL, state TEXT NOT NULL,
                   drive_id TEXT, cloud_name TEXT, cloud_parent TEXT, md5 TEXT, cloud_size INTEGER,
                   local_size INTEGER, local_mtime_ns INTEGER, inode INTEGER, upload_url TEXT,
                   upload_md5 TEXT, pin_prev INTEGER NOT NULL DEFAULT 0,
                   move_pending INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0,
                   retry_at INTEGER, error TEXT, updated_at INTEGER NOT NULL);
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta VALUES ('schema_version', '1');
                 INSERT INTO entries (path, kind, state, md5, updated_at) VALUES ('a', 'file', 'synced', 'x', 1234);",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.get("a").unwrap().unwrap().uploaded_at, Some(1234));
        assert_eq!(db.meta_get("schema_version").unwrap().as_deref(), Some("2"));
        db.log_activity(ActivityKind::Uploaded, "a", None, None).unwrap();
        // Reopening an up-to-date database is a no-op.
        drop(db);
        Db::open(&path).unwrap();
    }

    #[test]
    fn bind_root_refuses_other_folder() {
        let (_d, db) = db();
        db.bind_root(Path::new("/home/a/CloudDrop")).unwrap();
        db.bind_root(Path::new("/home/a/CloudDrop")).unwrap();
        assert!(db.bind_root(Path::new("/home/a/Other")).is_err());
    }

    #[test]
    fn ready_jobs_orders_dirs_first_and_honours_retry() {
        let (_d, db) = db();
        let mut f = Entry::new("a/file", Kind::File, None);
        db.put(&f).unwrap();
        db.put(&Entry::new("a", Kind::Dir, None)).unwrap();
        f.path = "later".into();
        f.retry_at = Some(now_ms() + 60_000);
        db.put(&f).unwrap();
        let jobs: Vec<_> = db
            .ready_jobs(now_ms(), 10)
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(jobs, ["a", "a/file"]);
    }
}

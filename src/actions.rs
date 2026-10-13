//! CLI operations on the sync folder: free up space, restore, open in Drive, status.

use crate::config::Config;
use crate::db::{ActivityKind, Db, Entry, Kind, State};
use crate::drive::{Drive, DriveError};
use crate::util::{self, LocalMeta};
use anyhow::{Context, Result, anyhow, bail};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

pub struct Ctx {
    pub cfg: Config,
    pub db: Db,
}

impl Ctx {
    pub fn load() -> Result<Self> {
        let cfg = Config::load()?;
        let db = Db::open_default()?;
        db.bind_root(&cfg.sync_dir)?;
        Ok(Self { cfg, db })
    }

    fn root(&self) -> &Path {
        &self.cfg.sync_dir
    }

    fn abs(&self, rel: &str) -> PathBuf {
        self.root().join(rel)
    }

    /// Turns a user path (absolute or relative to cwd) into an entry path; "" = the root.
    pub fn resolve(&self, arg: &Path) -> Result<String> {
        let full = if arg.is_absolute() {
            arg.to_path_buf()
        } else {
            std::env::current_dir()?.join(arg)
        };
        let full = normalize(&full);
        if full.strip_prefix(self.root()).is_err() {
            bail!(
                "{} is not inside the CloudDrop folder ({})",
                full.display(),
                self.root().display()
            );
        }
        util::rel_path(self.root(), &full)
            .ok_or_else(|| anyhow!("{} is not valid UTF-8", full.display()))
    }
}

/// Lexically resolves `.` and `..` (paths may name cloud-only items that don't exist).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub fn drive_url(kind: Kind, id: &str) -> String {
    match kind {
        Kind::Dir => format!("https://drive.google.com/drive/folders/{id}"),
        Kind::File => format!("https://drive.google.com/file/d/{id}/view"),
    }
}

/// Drive URL for an entry path ("" = the top CloudDrop folder).
pub fn url_for(ctx: &Ctx, rel: &str) -> Result<String> {
    if rel.is_empty() {
        let id = ctx
            .db
            .meta_get("root_id")?
            .ok_or_else(|| anyhow!("the CloudDrop folder has not been created in Drive yet"))?;
        return Ok(drive_url(Kind::Dir, &id));
    }
    let e = ctx
        .db
        .get(rel)?
        .ok_or_else(|| anyhow!("{rel} is not tracked by CloudDrop yet"))?;
    let id = e
        .drive_id
        .ok_or_else(|| anyhow!("{rel} has not been uploaded yet"))?;
    Ok(drive_url(e.kind, &id))
}

fn tracked_subtree(ctx: &Ctx, rel: &str) -> Result<Vec<Entry>> {
    let rows = ctx.db.subtree(rel)?;
    if rows.is_empty() && !rel.is_empty() {
        bail!("{rel} is not tracked by CloudDrop yet");
    }
    Ok(rows)
}

#[derive(Default, Debug)]
pub struct FreeReport {
    pub freed_files: u64,
    pub freed_bytes: u64,
    pub not_uploaded: u64,
    pub changed: u64,
    pub cloud_mismatch: u64,
}

impl FreeReport {
    pub fn summary(&self) -> String {
        let mut s = if self.freed_files == 0 {
            "Nothing to free.".to_string()
        } else {
            format!(
                "Freed {} ({} file{}). The files stay in Google Drive.",
                util::human_bytes(self.freed_bytes),
                self.freed_files,
                if self.freed_files == 1 { "" } else { "s" }
            )
        };
        let mut skipped = Vec::new();
        if self.not_uploaded > 0 {
            skipped.push(format!("{} not uploaded yet", self.not_uploaded));
        }
        if self.changed > 0 {
            skipped.push(format!("{} changed since upload", self.changed));
        }
        if self.cloud_mismatch > 0 {
            skipped.push(format!("{} missing or different in Drive (queued for upload)", self.cloud_mismatch));
        }
        if !skipped.is_empty() {
            s.push_str(&format!(" Kept: {}.", skipped.join(", ")));
        }
        s
    }
}

/// Deletes local copies that are verifiably identical to their Drive copy.
pub async fn free(ctx: &Ctx, drive: &Drive, rels: &[String]) -> Result<FreeReport> {
    let mut report = FreeReport::default();
    let mut files = Vec::new();
    for rel in rels {
        files.extend(
            tracked_subtree(ctx, rel)?
                .into_iter()
                .filter(|e| e.kind == Kind::File && e.state != State::CloudOnly),
        );
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);

    for mut e in files {
        if e.state != State::Synced {
            report.not_uploaded += 1;
            continue;
        }
        let abs = ctx.abs(&e.path);
        let path = abs.clone();
        let hashed = tokio::task::spawn_blocking(move || util::md5_file(&path)).await?;
        let Ok((md5, meta)) = hashed else {
            continue; // already gone
        };
        if Some(meta) != e.local() || Some(&md5) != e.md5.as_ref() {
            report.changed += 1;
            continue;
        }
        let id = e.drive_id.clone().expect("synced files have a Drive id");
        let cloud = match drive.get(&id).await {
            Ok(m) if !m.trashed => Some(m),
            Ok(_) | Err(DriveError::NotFound) => None,
            Err(err) => return Err(anyhow!("cannot check {} in Drive: {err}", e.path)),
        };
        let cloud_ok = cloud.as_ref().is_some_and(|m| {
            m.md5_checksum.as_deref() == Some(md5.as_str()) && m.size_bytes() == Some(meta.size as u64)
        });
        if !cloud_ok {
            // Drive lost or changed it: upload the local copy again rather than free it.
            report.cloud_mismatch += 1;
            crate::engine::mark_for_reupload(&mut e, cloud.is_some());
            ctx.db.put(&e)?;
            continue;
        }
        // Re-check right before deleting, in case it was edited during the Drive call.
        if LocalMeta::read(&abs).ok() != Some(meta) {
            report.changed += 1;
            continue;
        }
        crate::store::platform()
            .free(&abs, &id)
            .with_context(|| format!("cannot free {}", abs.display()))?;
        e.state = State::CloudOnly;
        ctx.db.put(&e)?;
        let _ = ctx.db.log_activity(ActivityKind::Freed, &e.path, Some(meta.size), None);
        report.freed_files += 1;
        report.freed_bytes += meta.size as u64;
    }
    Ok(report)
}

#[derive(Default, Debug)]
pub struct RestoreReport {
    pub files: u64,
    pub bytes: u64,
    pub blocked: Vec<String>,
}

impl RestoreReport {
    pub fn summary(&self) -> String {
        let mut s = if self.files == 0 {
            "Nothing to download.".to_string()
        } else {
            format!(
                "Downloaded {} ({} file{}).",
                util::human_bytes(self.bytes),
                self.files,
                if self.files == 1 { "" } else { "s" }
            )
        };
        if !self.blocked.is_empty() {
            s.push_str(&format!(
                " Skipped {} because another file is already there.",
                self.blocked.join(", ")
            ));
        }
        s
    }
}

/// Re-activates cloud-only folders above `rel` that now exist locally again.
fn reactivate_ancestors(ctx: &Ctx, rel: &str) -> Result<()> {
    let mut p = util::parent_of(rel);
    while !p.is_empty() {
        if let Some(mut d) = ctx.db.get(p)?
            && d.state == State::CloudOnly
        {
            d.requeue();
            ctx.db.put(&d)?;
        }
        p = util::parent_of(p);
    }
    Ok(())
}

/// Downloads cloud-only files (and recreates cloud-only folders) under each path.
pub async fn restore(ctx: &Ctx, drive: &Drive, rels: &[String]) -> Result<RestoreReport> {
    let mut report = RestoreReport::default();
    for rel in rels {
        for mut e in tracked_subtree(ctx, rel)? {
            if e.state != State::CloudOnly {
                continue;
            }
            match e.kind {
                Kind::Dir => {
                    std::fs::create_dir_all(ctx.abs(&e.path))?;
                    reactivate_ancestors(ctx, &e.path)?;
                    e.requeue();
                    ctx.db.put(&e)?;
                }
                Kind::File => {
                    let abs = ctx.abs(&e.path);
                    // A placeholder without data (Windows) is replaced by the download.
                    if std::fs::symlink_metadata(&abs).is_ok()
                        && !crate::store::platform().is_dehydrated(&abs)
                    {
                        report.blocked.push(e.path.clone());
                        continue;
                    }
                    let n = restore_file(ctx, drive, e).await?;
                    report.files += 1;
                    report.bytes += n;
                }
            }
        }
    }
    Ok(report)
}

async fn restore_file(ctx: &Ctx, drive: &Drive, mut e: Entry) -> Result<u64> {
    let id = e
        .drive_id
        .clone()
        .ok_or_else(|| anyhow!("{} has no Drive copy", e.path))?;
    let cloud = drive
        .get(&id)
        .await
        .map_err(|err| anyhow!("cannot find {} in Drive: {err}", e.path))?;
    let dest = ctx.abs(&e.path);
    let dir = dest.parent().expect("inside root").to_path_buf();
    std::fs::create_dir_all(&dir)?;
    reactivate_ancestors(ctx, &e.path)?;

    let tmp = dir.join(format!(".clouddrop-restore-{:016x}.part", rand::random::<u64>()));
    let result: Result<u64> = async {
        let mut out = tokio::fs::File::create(&tmp).await?;
        let (md5, n) = drive
            .download(&id, &mut out)
            .await
            .map_err(|err| anyhow!("download of {} failed: {err}", e.path))?;
        if cloud.md5_checksum.as_deref().is_some_and(|c| c != md5) {
            bail!("download of {} was corrupted (checksum mismatch)", e.path);
        }
        let out = out.into_std().await;
        if let Some(ns) = e.local_mtime_ns.filter(|ns| *ns > 0) {
            let when = SystemTime::UNIX_EPOCH + Duration::from_nanos(ns as u64);
            let _ = out.set_modified(when);
        }
        out.sync_all()?;
        drop(out);

        // Record first, then move into place: the daemon then sees a known, synced file.
        e.md5 = Some(md5);
        e.cloud_size = Some(n as i64);
        e.set_local(Some(LocalMeta::read(&tmp)?));
        e.state = if e.move_pending { State::Pending } else { State::Synced };
        e.move_pending = false;
        e.pin_prev = false;
        e.error = None;
        // Downloaded on purpose: automatic space saving leaves it alone until it
        // changes and is uploaded again.
        let uploaded_at = e.uploaded_at.take();
        ctx.db.put(&e)?;
        if let Err(err) = std::fs::rename(&tmp, &dest) {
            e.state = State::CloudOnly;
            e.uploaded_at = uploaded_at;
            ctx.db.put(&e)?;
            return Err(anyhow!("cannot move download into place: {err}"));
        }
        let _ = ctx.db.log_activity(ActivityKind::Restored, &e.path, Some(n as i64), None);
        if e.state == State::Synced {
            crate::store::platform().synced(&dest, &id, false);
        }
        Ok(n)
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Cloud-only items directly inside `rel`, folders first.
pub fn cloud_only_children(ctx: &Ctx, rel: &str) -> Result<Vec<Entry>> {
    let mut v: Vec<Entry> = ctx
        .db
        .children(rel)?
        .into_iter()
        .filter(|e| e.state == State::CloudOnly)
        .collect();
    v.sort_by(|a, b| (a.kind != Kind::Dir, &a.path).cmp(&(b.kind != Kind::Dir, &b.path)));
    Ok(v)
}

/// Forgets every Drive id so everything is uploaded again into a fresh top folder.
/// Used when the CloudDrop folder was permanently deleted from Drive.
pub fn reset_cloud(ctx: &Ctx) -> Result<(usize, usize)> {
    let mut requeued = 0;
    let mut dropped = 0;
    for mut e in ctx.db.subtree("")? {
        if e.state == State::CloudOnly {
            ctx.db.delete(&e.path)?;
            dropped += 1;
            continue;
        }
        e.drive_id = None;
        e.md5 = None;
        e.cloud_size = None;
        e.cloud_name = None;
        e.cloud_parent = None;
        e.upload_url = None;
        e.upload_md5 = None;
        e.pin_prev = false;
        e.move_pending = false;
        e.requeue();
        ctx.db.put(&e)?;
        requeued += 1;
    }
    ctx.db.meta_delete("root_id")?;
    Ok((requeued, dropped))
}

/// Adds the sync folder to the Files sidebar (GTK bookmarks) if it isn't there.
pub fn add_bookmark(dir: &Path) -> Result<bool> {
    let file = dirs::config_dir()
        .ok_or_else(|| anyhow!("no config dir"))?
        .join("gtk-3.0/bookmarks");
    let url = url::Url::from_file_path(dir)
        .map_err(|_| anyhow!("bad folder path"))?
        .to_string();
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    if existing
        .lines()
        .any(|l| l.split(' ').next() == Some(url.as_str()))
    {
        return Ok(false);
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let label = dir.file_name().and_then(|n| n.to_str()).unwrap_or("CloudDrop");
    text.push_str(&format!("{url} {label}\n"));
    std::fs::write(&file, text)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_lexically() {
        assert_eq!(normalize(Path::new("/a/b/../c/./d")), PathBuf::from("/a/c/d"));
    }

    #[test]
    fn urls() {
        assert_eq!(drive_url(Kind::Dir, "X"), "https://drive.google.com/drive/folders/X");
        assert_eq!(drive_url(Kind::File, "Y"), "https://drive.google.com/file/d/Y/view");
    }
}

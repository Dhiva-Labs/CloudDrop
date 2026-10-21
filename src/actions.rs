//! CLI operations on the sync folder: free up space, restore, open in Drive, status.

use crate::config::Config;
use crate::db::{ActivityKind, Db, Entry, Kind, State};
use crate::engine::{describe, short_name};
use crate::jobs::{hash_file, local_kind, proven};
use crate::remote::{Capabilities, Provider, Remote, RemoteError};
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
        Self::load_for(Config::load()?, Db::open_default()?)
    }

    /// A connection's config and database (checked to belong to its folder).
    pub fn load_for(cfg: Config, db: Db) -> Result<Self> {
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

/// Web page of an entry path in the cloud ("" = the top CloudDrop folder).
pub fn url_for(ctx: &Ctx, rel: &str) -> Result<String> {
    if rel.is_empty() {
        let id = ctx.db.meta_get("root_id")?.ok_or_else(|| {
            anyhow!("the CloudDrop folder has not been created in {} yet", short_name(ctx.cfg.provider))
        })?;
        return web_url(ctx, Kind::Dir, &id);
    }
    let e = ctx
        .db
        .get(rel)?
        .ok_or_else(|| anyhow!("{rel} is not tracked by CloudDrop yet"))?;
    let id = e
        .drive_id
        .ok_or_else(|| anyhow!("{rel} has not been uploaded yet"))?;
    web_url(ctx, e.kind, &id)
}

fn web_url(ctx: &Ctx, kind: Kind, id: &str) -> Result<String> {
    let p = ctx.cfg.provider;
    p.web_url(kind, id)
        .ok_or_else(|| anyhow!("{} has no web page for files", p.display_name()))
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
    /// Where the files stay.
    pub provider: Provider,
    pub freed_files: u64,
    pub freed_bytes: u64,
    /// Handed to the running service (Windows: only it may free files).
    pub handed_files: u64,
    pub handed_bytes: u64,
    pub not_uploaded: u64,
    pub changed: u64,
    pub cloud_mismatch: u64,
}

impl FreeReport {
    pub fn summary(&self) -> String {
        let name = self.provider.display_name();
        let mut s = if self.handed_files > 0 {
            format!(
                "Freeing {} ({} file{}) — CloudDrop does it in a moment. The files stay in {name}.",
                util::human_bytes(self.freed_bytes + self.handed_bytes),
                self.freed_files + self.handed_files,
                if self.freed_files + self.handed_files == 1 { "" } else { "s" }
            )
        } else if self.freed_files == 0 {
            "Nothing to free.".to_string()
        } else {
            format!(
                "Freed {} ({} file{}). The files stay in {name}.",
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
            skipped.push(format!(
                "{} missing or different in {} (queued for upload)",
                self.cloud_mismatch,
                short_name(self.provider)
            ));
        }
        if !skipped.is_empty() {
            s.push_str(&format!(" Kept: {}.", skipped.join(", ")));
        }
        s
    }
}

/// Deletes local copies that are verifiably identical to their cloud copy: the provider's
/// hash matches, or for providers without one, a download of the cloud copy does.
pub async fn free(ctx: &Ctx, remote: &Remote, rels: &[String]) -> Result<FreeReport> {
    free_with(ctx, remote, remote.caps(), rels).await
}

/// [`free`] as if the provider had `caps` (tests apply other providers' rules to the Drive mock).
pub async fn free_with(ctx: &Ctx, remote: &Remote, caps: &Capabilities, rels: &[String]) -> Result<FreeReport> {
    let provider = remote.provider();
    let kind = local_kind(caps);
    let mut report = FreeReport { provider, ..Default::default() };
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
        let hashed = tokio::task::spawn_blocking(move || hash_file(&path, kind)).await?;
        let Ok((hash, meta)) = hashed else {
            continue; // already gone
        };
        if Some(meta) != e.local() || Some(&hash) != e.content_hash().as_ref() {
            report.changed += 1;
            continue;
        }
        let id = e.drive_id.clone().expect("synced files have a Drive id");
        let checked = match remote.get(&id).await {
            Ok(m) if !m.trashed => {
                // Verify by download, where needed, next to the file (an ignored temp name).
                let tmp_dir = abs.parent().unwrap_or(ctx.root()).to_path_buf();
                match proven(remote, caps, &m, &hash, meta.size as u64, &tmp_dir).await {
                    Ok(same) => Some(same),
                    Err(RemoteError::NotFound) => None,
                    Err(err) => return Err(anyhow!("cannot check {}: {}", e.path, describe(&err, provider))),
                }
            }
            Ok(_) | Err(RemoteError::NotFound) => None,
            Err(err) => return Err(anyhow!("cannot check {}: {}", e.path, describe(&err, provider))),
        };
        if checked.is_none() && !caps.stable_ids && ctx.db.get(&e.path)?.and_then(|now| now.drive_id) != Some(id.clone()) {
            // Its folder was just renamed (ids are paths): check it again later.
            report.changed += 1;
            continue;
        }
        if checked != Some(true) {
            // The cloud lost or changed it: upload the local copy again rather than free it.
            report.cloud_mismatch += 1;
            crate::engine::mark_for_reupload(&mut e, checked.is_some());
            ctx.db.put(&e)?;
            continue;
        }
        // Re-check right before deleting, in case it was edited during the cloud check.
        if LocalMeta::read(&abs).ok() != Some(meta) {
            report.changed += 1;
            continue;
        }
        let freed_now = crate::store::platform()
            .request_free(&abs, &id)
            .with_context(|| format!("cannot free {}", abs.display()))?;
        if !freed_now {
            report.handed_files += 1;
            report.handed_bytes += meta.size as u64;
            continue;
        }
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
    /// Still downloading (through the service) when the command stopped waiting.
    pub downloading: u64,
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
        if self.downloading > 0 {
            s.push_str(&format!(" {} still downloading in the background.", self.downloading));
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
pub async fn restore(ctx: &Ctx, remote: &Remote, rels: &[String]) -> Result<RestoreReport> {
    restore_with(ctx, remote, remote.caps(), rels).await
}

/// [`restore`] as if the provider had `caps`.
pub async fn restore_with(ctx: &Ctx, remote: &Remote, caps: &Capabilities, rels: &[String]) -> Result<RestoreReport> {
    let mut report = RestoreReport::default();
    let store = crate::store::platform();
    // Placeholders (Windows) the service is downloading: (path, entry path, size).
    let mut waiting: Vec<(PathBuf, String, i64)> = Vec::new();
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
                    if store.is_dehydrated(&abs) {
                        // Windows: the agent downloads it, so it stays a cloud file
                        // in Explorer.
                        if !crate::control::daemon_running() {
                            bail!("CloudDrop isn't running: start it to download {}", e.path);
                        }
                        crate::control::request_hydrate(&ctx.db, &e.path)?;
                        waiting.push((abs, e.path.clone(), e.cloud_size.unwrap_or(0)));
                        continue;
                    }
                    if std::fs::symlink_metadata(&abs).is_ok() {
                        report.blocked.push(e.path.clone());
                        continue;
                    }
                    let n = restore_file(ctx, remote, caps, e).await?;
                    report.files += 1;
                    report.bytes += n;
                }
            }
        }
    }
    if !waiting.is_empty() {
        // Wait for the downloads, allowing at least 1 MB/s.
        let total: i64 = waiting.iter().map(|w| w.2).sum();
        let deadline = std::time::Instant::now() + Duration::from_secs(60 + total as u64 / 1_000_000);
        loop {
            waiting.retain(|(abs, rel, size)| {
                if store.is_dehydrated(abs) {
                    return true;
                }
                let _ = ctx.db.log_activity(ActivityKind::Restored, rel, Some(*size), None);
                report.files += 1;
                report.bytes += *size as u64;
                false
            });
            if waiting.is_empty() || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        report.downloading = waiting.len() as u64;
    }
    Ok(report)
}

async fn restore_file(ctx: &Ctx, remote: &Remote, caps: &Capabilities, mut e: Entry) -> Result<u64> {
    let provider = remote.provider();
    let id = e
        .drive_id
        .clone()
        .ok_or_else(|| anyhow!("{} has no copy in {}", e.path, short_name(provider)))?;
    let cloud = remote
        .get(&id)
        .await
        .map_err(|err| anyhow!("cannot find {}: {}", e.path, describe(&err, provider)))?;
    let dest = ctx.abs(&e.path);
    let dir = dest.parent().expect("inside root").to_path_buf();
    std::fs::create_dir_all(&dir)?;
    reactivate_ancestors(ctx, &e.path)?;

    let tmp = dir.join(format!(".clouddrop-restore-{:016x}.part", rand::random::<u64>()));
    let result: Result<u64> = async {
        let mut out = tokio::fs::File::create(&tmp).await?;
        let (hash, n) = remote
            .download(&id, &mut out)
            .await
            .map_err(|err| anyhow!("download of {} failed: {}", e.path, describe(&err, provider)))?;
        if cloud.hash.as_ref().is_some_and(|c| c.kind == hash.kind && *c != hash) {
            bail!("download of {} was corrupted (checksum mismatch)", e.path);
        }
        // Recorded in the kind local files are hashed in for this provider.
        let kind = local_kind(caps);
        let hash = if hash.kind == kind {
            hash
        } else {
            let path = tmp.clone();
            tokio::task::spawn_blocking(move || hash_file(&path, kind)).await??.0
        };
        let out = out.into_std().await;
        if let Some(ns) = e.local_mtime_ns.filter(|ns| *ns > 0) {
            let when = SystemTime::UNIX_EPOCH + Duration::from_nanos(ns as u64);
            let _ = out.set_modified(when);
        }
        out.sync_all()?;
        drop(out);

        // Record first, then move into place: the daemon then sees a known, synced file.
        e.set_content_hash(Some(hash));
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

/// Forgets every cloud id so everything is uploaded again into a fresh top folder, and binds the
/// database to the configured provider. Used when the CloudDrop folder was permanently deleted
/// from the cloud, and to switch to another provider (nothing is deleted in the old one).
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
        e.set_content_hash(None);
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
    ctx.db.meta_set("provider", ctx.cfg.provider.as_str())?;
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
        let p = crate::remote::Provider::GoogleDrive;
        assert_eq!(p.web_url(Kind::Dir, "X").as_deref(), Some("https://drive.google.com/drive/folders/X"));
        assert_eq!(p.web_url(Kind::File, "Y").as_deref(), Some("https://drive.google.com/file/d/Y/view"));
    }
}

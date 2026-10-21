//! One unit of Drive work for one entry. Jobs never touch the database: they report
//! back to the engine, which is the single writer and knows where the entry is now.
//!
//! Every step is safe to interrupt: uploads go through resumable sessions whose URL
//! the engine persists, and creation first looks for an item it may already have
//! made (crash between "created in Drive" and "recorded locally").
//!
//! Content is compared by hash, in the kind the provider reports (`caps.hash`). Where it
//! reports none, local files are hashed with SHA-256 and the cloud copy is proven by
//! downloading it ("verify by download") before anything relies on it.

use crate::db::{Entry, Kind};
use crate::remote::{Capabilities, ContentHash, HashKind, Remote, RemoteError, RemoteMeta, RemoteResult, UploadStatus};
use crate::util::LocalMeta;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

pub struct JobSpec {
    pub id: u64,
    pub abs: PathBuf,
    /// Snapshot of the entry when the job was scheduled.
    pub entry: Entry,
    /// Where the item belongs in Drive.
    pub name: String,
    pub parent_id: String,
    /// Only fix name/parent (a cloud-only entry that was renamed locally).
    pub placement_only: bool,
    /// Check that Drive holds this exact content so the local copy can be freed.
    pub free: bool,
    pub chunk: usize,
    /// How the provider behaves (its own capabilities, or a test's stand-in).
    pub caps: Capabilities,
    /// Where verify-by-download keeps its temporary file.
    pub tmp_dir: PathBuf,
}

#[derive(Debug)]
pub struct JobDone {
    pub drive_id: String,
    /// The id before this job renamed or moved the item, where that changed it (paths as
    /// ids): the ids of everything beneath it changed too.
    pub old_id: Option<String>,
    pub cloud_name: String,
    pub cloud_parent: String,
    /// Files: hash of the content in the cloud, in the kind local files are hashed in
    /// ([`local_kind`]).
    pub hash: Option<ContentHash>,
    pub size: Option<i64>,
    /// Files: local metadata of the content that was hashed/uploaded.
    pub local: Option<LocalMeta>,
    /// Files: the cloud's hash (where it reports one) and size equal what we computed before
    /// uploading.
    pub content_ok: bool,
    pub uploaded_bytes: u64,
}

#[derive(Debug)]
pub enum JobOutcome {
    Done(JobDone),
    /// Free job: Drive has identical content; `local` is the file as it was checked.
    FreeReady(LocalMeta),
    /// Free job: keep the local copy.
    FreeSkip(FreeSkip),
    /// The local file was gone before the job could read it.
    Vanished,
    Failed(RemoteError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeSkip {
    /// The local file changed since it was uploaded.
    LocalChanged,
    /// The Drive file exists but holds different content (edited elsewhere).
    CloudDiffers,
    /// The Drive file was deleted or trashed.
    CloudGone,
}

/// Messages a running job sends to the engine.
pub enum JobEvent {
    /// An upload session was opened for content with this hash (its value).
    Session { id: u64, url: String, md5: String },
    /// Account storage: (limit, usage, usage in Drive).
    Quota(Option<u64>, u64, u64),
    /// Bytes of a file upload Drive has confirmed so far.
    Progress { id: u64, sent: u64, total: u64 },
    Finished { id: u64, outcome: JobOutcome },
}

pub async fn run(spec: JobSpec, remote: Arc<Remote>, tx: UnboundedSender<JobEvent>) {
    let id = spec.id;
    let outcome = if spec.free {
        free_job(&spec, &remote).await
    } else if spec.placement_only {
        placement_job(&spec, &remote).await
    } else {
        match spec.entry.kind {
            Kind::Dir => dir_job(&spec, &remote).await,
            Kind::File => file_job(&spec, &remote, &tx).await,
        }
    };
    let _ = tx.send(JobEvent::Finished { id, outcome });
}

/// The kind local files are hashed in for a provider: its own, or SHA-256 where it reports
/// none (to compare with a download).
pub fn local_kind(caps: &Capabilities) -> HashKind {
    caps.hash.unwrap_or(HashKind::Sha256)
}

/// Hashes a file in `kind`: (hash, metadata before reading).
pub fn hash_file(path: &Path, kind: HashKind) -> std::io::Result<(ContentHash, LocalMeta)> {
    let mut f = std::fs::File::open(path)?;
    let meta = LocalMeta::of_file(&f)?;
    Ok((crate::hash::hash_reader(kind, &mut f)?, meta))
}

const VERIFY_PREFIX: &str = ".clouddrop-verify-";

/// Removes temporary downloads a crash left behind in `dir`.
pub fn remove_stale_downloads(dir: &Path) {
    for ent in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if ent.file_name().to_string_lossy().starts_with(VERIFY_PREFIX) {
            let _ = std::fs::remove_file(ent.path());
        }
    }
}

/// Verify by download: fetches the cloud file `id` into a temporary file in `tmp_dir` and
/// checks that it holds exactly `want` (`size` bytes). The temporary file is always removed.
pub async fn cloud_copy_matches(
    remote: &Remote,
    id: &str,
    want: &ContentHash,
    size: u64,
    tmp_dir: &Path,
) -> RemoteResult<bool> {
    let io = |e: std::io::Error| RemoteError::Permanent(format!("cannot keep a temporary download: {e}"));
    tokio::fs::create_dir_all(tmp_dir).await.map_err(io)?;
    let tmp = tmp_dir.join(format!("{VERIFY_PREFIX}{:016x}.part", rand::random::<u64>()));
    let result = async {
        let mut out = tokio::fs::File::create(&tmp).await.map_err(io)?;
        let (got, n) = remote.download(id, &mut out).await?;
        drop(out);
        if n != size {
            return Ok(false);
        }
        let got = if got.kind == want.kind {
            got
        } else {
            let (path, kind) = (tmp.clone(), want.kind);
            tokio::task::spawn_blocking(move || hash_file(&path, kind))
                .await
                .map_err(|e| RemoteError::Permanent(format!("hash task failed: {e}")))?
                .map_err(io)?
                .0
        };
        Ok(got == *want)
    }
    .await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

/// The cloud file `meta` holds exactly `hash` (`size` bytes): by the provider's hash, or by
/// downloading it where the provider reports none for it.
pub async fn proven(
    remote: &Remote,
    caps: &Capabilities,
    meta: &RemoteMeta,
    hash: &ContentHash,
    size: u64,
    tmp_dir: &Path,
) -> RemoteResult<bool> {
    if caps.hash.is_some() && meta.hash.is_some() {
        return Ok(meta.same_content(hash, size));
    }
    if meta.size.is_some_and(|s| s != size) {
        return Ok(false);
    }
    cloud_copy_matches(remote, &meta.id, hash, size, tmp_dir).await
}

/// A finished rename/move: `id` is the item's id afterwards, `before` its id when the job started.
fn done_meta(spec: &JobSpec, id: String, before: Option<&str>) -> JobDone {
    JobDone {
        old_id: before.filter(|b| *b != id).map(str::to_string),
        drive_id: id,
        cloud_name: spec.name.clone(),
        cloud_parent: spec.parent_id.clone(),
        hash: spec.entry.content_hash(),
        size: spec.entry.cloud_size,
        local: None,
        content_ok: true,
        uploaded_bytes: 0,
    }
}

/// Renames/moves `id` so it is called `spec.name` inside `spec.parent_id`; returns its id
/// afterwards (a new one where ids are paths).
async fn place(remote: &Remote, id: &str, spec: &JobSpec) -> RemoteResult<String> {
    let e = &spec.entry;
    let rename = e.cloud_name.as_deref() != Some(spec.name.as_str());
    let moved = e.cloud_parent.as_deref() != Some(spec.parent_id.as_str());
    if !rename && !moved {
        return Ok(id.to_string());
    }
    let old_parent = match (&e.cloud_parent, moved) {
        (_, false) => None,
        (Some(p), true) => Some(p.clone()),
        (None, true) => remote.get(id).await?.parents.first().cloned(),
    };
    let meta = remote
        .move_rename(
            id,
            rename.then_some(spec.name.as_str()),
            old_parent
                .as_deref()
                .map(|old| (spec.parent_id.as_str(), old)),
        )
        .await?;
    Ok(if spec.caps.stable_ids || meta.id.is_empty() { id.to_string() } else { meta.id })
}

async fn placement_job(spec: &JobSpec, remote: &Remote) -> JobOutcome {
    let Some(id) = spec.entry.drive_id.clone() else {
        return JobOutcome::Failed(RemoteError::Permanent("nothing in the cloud to move".into()));
    };
    match place(remote, &id, spec).await {
        Ok(new) => JobOutcome::Done(done_meta(spec, new, Some(&id))),
        Err(e) => JobOutcome::Failed(e),
    }
}

/// Re-hashes the local file and proves the cloud holds the same content (the provider's
/// hash, or a download where it has none); never deletes anything itself.
async fn free_job(spec: &JobSpec, remote: &Remote) -> JobOutcome {
    let (Some(id), Some(_)) = (spec.entry.drive_id.clone(), spec.entry.md5.as_ref()) else {
        return JobOutcome::FreeSkip(FreeSkip::CloudGone);
    };
    let kind = local_kind(&spec.caps);
    let Some(expected) = spec.entry.content_hash().filter(|h| h.kind == kind) else {
        // A hash of another kind proves nothing: upload again, keeping the cloud version.
        return JobOutcome::FreeSkip(FreeSkip::CloudDiffers);
    };
    let abs = spec.abs.clone();
    let hashed = tokio::task::spawn_blocking(move || hash_file(&abs, kind)).await;
    let (hash, local) = match hashed {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return io_failure(e, "read the file"),
        Err(e) => return JobOutcome::Failed(RemoteError::Permanent(format!("hash task failed: {e}"))),
    };
    if hash != expected {
        return JobOutcome::FreeSkip(FreeSkip::LocalChanged);
    }
    let meta = match remote.get(&id).await {
        Ok(m) if !m.trashed => m,
        Ok(_) | Err(RemoteError::NotFound) => return JobOutcome::FreeSkip(FreeSkip::CloudGone),
        Err(e) => return JobOutcome::Failed(e),
    };
    match proven(remote, &spec.caps, &meta, &hash, local.size as u64, &spec.tmp_dir).await {
        Ok(true) => JobOutcome::FreeReady(local),
        Ok(false) => JobOutcome::FreeSkip(FreeSkip::CloudDiffers),
        Err(RemoteError::NotFound) => JobOutcome::FreeSkip(FreeSkip::CloudGone),
        Err(e) => JobOutcome::Failed(e),
    }
}

async fn dir_job(spec: &JobSpec, remote: &Remote) -> JobOutcome {
    if let Some(id) = spec.entry.drive_id.clone() {
        match place(remote, &id, spec).await {
            Ok(new) => return JobOutcome::Done(done_meta(spec, new, Some(&id))),
            // Folder deleted in Drive: make it again below.
            Err(RemoteError::NotFound) => {}
            Err(e) => return JobOutcome::Failed(e),
        }
    }
    let result = async {
        if let Some(existing) = remote
            .find_child(&spec.name, &spec.parent_id, true)
            .await?
            .into_iter()
            .next()
        {
            return Ok(existing.id);
        }
        Ok(remote.create_folder(&spec.name, &spec.parent_id).await?.id)
    }
    .await;
    match result {
        Ok(id) => JobOutcome::Done(done_meta(spec, id, None)),
        Err(e) => JobOutcome::Failed(e),
    }
}

fn io_failure(e: std::io::Error, what: &str) -> JobOutcome {
    if e.kind() == std::io::ErrorKind::NotFound {
        JobOutcome::Vanished
    } else {
        JobOutcome::Failed(RemoteError::Permanent(format!("cannot {what}: {e}")))
    }
}

async fn file_job(spec: &JobSpec, remote: &Remote, tx: &UnboundedSender<JobEvent>) -> JobOutcome {
    // Hold one descriptor for hashing and uploading, so a rename or delete mid-way
    // cannot make us upload something other than what we hashed.
    let abs = spec.abs.clone();
    let caps = &spec.caps;
    let kind = local_kind(caps);
    let hashed = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        let mut f = std::fs::File::open(&abs)?;
        let md = f.metadata()?;
        if !md.is_file() {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not a file"));
        }
        let meta = LocalMeta::of_file(&f)?;
        let hash = crate::hash::hash_reader(kind, &mut f)?;
        Ok((f, hash, meta))
    })
    .await;
    let (file, hash, local) = match hashed {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return io_failure(e, "read the file"),
        Err(e) => return JobOutcome::Failed(RemoteError::Permanent(format!("hash task failed: {e}"))),
    };
    let size = local.size as u64;
    let e = &spec.entry;
    let report = |sent: u64| {
        let _ = tx.send(JobEvent::Progress { id: spec.id, sent, total: size });
    };
    report(0);
    let mut drive_id = e.drive_id.clone();
    let done = |id: String, meta: Option<&RemoteMeta>, uploaded: u64| {
        // Where the provider reports no hash, the content is what was sent; sizes still agree.
        let cloud_hash = meta.and_then(|m| m.hash.clone()).filter(|_| caps.hash.is_some());
        let cloud_size = meta.and_then(|m| m.size);
        JobDone {
            drive_id: id,
            old_id: None,
            cloud_name: spec.name.clone(),
            cloud_parent: spec.parent_id.clone(),
            content_ok: cloud_hash.as_ref().is_none_or(|h| *h == hash) && cloud_size.is_none_or(|s| s == size),
            hash: Some(cloud_hash.unwrap_or_else(|| hash.clone())),
            size: Some(cloud_size.unwrap_or(size) as i64),
            local: Some(local),
            uploaded_bytes: uploaded,
        }
    };

    // Same content already in Drive: at most a rename/move.
    if let Some(id) = drive_id.clone()
        && e.content_hash().as_ref() == Some(&hash)
    {
        match place(remote, &id, spec).await {
            Ok(new) => {
                let old_id = (new != id).then_some(id);
                return JobOutcome::Done(JobDone { old_id, ..done(new, None, 0) });
            }
            Err(RemoteError::NotFound) => drive_id = None,
            Err(err) => return JobOutcome::Failed(err),
        }
    }

    // Replacing content: make sure the Drive file still exists and isn't trashed.
    if let Some(id) = drive_id.clone() {
        match remote.get(&id).await {
            Ok(m) if !m.trashed => {
                if e.pin_prev
                    && let Err(err) = remote.keep_version(&id).await
                {
                    return JobOutcome::Failed(err);
                }
            }
            Ok(_) | Err(RemoteError::NotFound) => drive_id = None,
            Err(err) => return JobOutcome::Failed(err),
        }
    }

    // New file: adopt an identical one already in the folder (crash recovery, lost database).
    if drive_id.is_none() {
        let found = match remote.find_child(&spec.name, &spec.parent_id, false).await {
            Ok(found) => found,
            Err(err) => return JobOutcome::Failed(err),
        };
        for m in &found {
            match proven(remote, caps, m, &hash, size, &spec.tmp_dir).await {
                Ok(true) => return JobOutcome::Done(done(m.id.clone(), Some(m), 0)),
                Ok(false) | Err(RemoteError::NotFound) => {}
                Err(err) => return JobOutcome::Failed(err),
            }
        }
    }

    let mime = mime_guess::from_path(&spec.name)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    let mut session = e
        .upload_url
        .clone()
        .filter(|_| e.upload_md5.as_deref() == Some(hash.value.as_str()));
    let mut file = Some(file);

    for _ in 0..3 {
        let mut offset = 0u64;
        let url = match session.take() {
            Some(url) => match remote.query_upload(&url, size).await {
                Ok(UploadStatus::Next(n)) => {
                    offset = n;
                    url
                }
                Ok(UploadStatus::Done(m)) => return finish(remote, caps, m, &done).await,
                Err(RemoteError::NotFound) => continue,
                Err(err) => return JobOutcome::Failed(err),
            },
            None => {
                let started = remote
                    .start_upload(
                        drive_id.as_deref(),
                        &spec.name,
                        (&spec.parent_id, e.cloud_parent.as_deref()),
                        size,
                        &mime,
                    )
                    .await;
                match started {
                    Ok(url) => {
                        let _ = tx.send(JobEvent::Session {
                            id: spec.id,
                            url: url.clone(),
                            md5: hash.value.clone(),
                        });
                        url
                    }
                    Err(RemoteError::NotFound) if drive_id.is_some() => {
                        drive_id = None;
                        continue;
                    }
                    Err(err) => return JobOutcome::Failed(err),
                }
            }
        };

        match send_chunks(remote, &url, &mut file, offset, size, spec.chunk, &report).await {
            Ok(m) => return finish(remote, caps, m, &done).await,
            // Session expired mid-way: start a fresh one.
            Err(RemoteError::NotFound) => continue,
            Err(err) => return JobOutcome::Failed(err),
        }
    }
    JobOutcome::Failed(RemoteError::Transient("upload session keeps expiring".into()))
}

async fn finish(
    remote: &Remote,
    caps: &Capabilities,
    mut meta: RemoteMeta,
    done: &impl Fn(String, Option<&RemoteMeta>, u64) -> JobDone,
) -> JobOutcome {
    if meta.hash.is_none() && caps.hash.is_some() {
        match remote.get(&meta.id).await {
            Ok(m) => meta = m,
            Err(err) => return JobOutcome::Failed(err),
        }
    }
    let uploaded = meta.size.unwrap_or(0);
    JobOutcome::Done(done(meta.id.clone(), Some(&meta), uploaded))
}

/// Uploads from `offset` to the end; returns Drive's metadata for the finished file.
async fn send_chunks(
    remote: &Remote,
    url: &str,
    file: &mut Option<std::fs::File>,
    mut offset: u64,
    size: u64,
    chunk: usize,
    report: &impl Fn(u64),
) -> RemoteResult<RemoteMeta> {
    let mut stalls = 0;
    loop {
        report(offset);
        let want = (size - offset.min(size)).min(chunk as u64) as usize;
        let f = file.take().expect("file handle");
        let (f, data) = tokio::task::spawn_blocking(move || {
            let mut f = f;
            let mut buf = vec![0u8; want];
            let r = f
                .seek(SeekFrom::Start(offset))
                .and_then(|_| f.read_exact(&mut buf));
            (f, r.map(|_| buf))
        })
        .await
        .map_err(|e| RemoteError::Permanent(format!("read task failed: {e}")))?;
        *file = Some(f);
        let data = data.map_err(|_| RemoteError::Transient("file changed while uploading".into()))?;

        match remote.put_chunk(url, bytes::Bytes::from(data), offset, size).await? {
            UploadStatus::Done(m) => {
                report(size);
                return Ok(m);
            }
            UploadStatus::Next(next) => {
                if next <= offset && want > 0 {
                    stalls += 1;
                    if stalls >= 3 {
                        return Err(RemoteError::Transient("upload is not progressing".into()));
                    }
                } else {
                    stalls = 0;
                }
                offset = next;
            }
        }
    }
}

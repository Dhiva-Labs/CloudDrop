//! One unit of cloud work for one entry. Jobs never touch the database: they report
//! back to the engine, which is the single writer and knows where the entry is now.
//!
//! Every step is safe to interrupt: uploads go through resumable sessions whose URL
//! the engine persists, and creation first looks for an item it may already have
//! made (crash between "created in the cloud" and "recorded locally").

use crate::db::{Entry, Kind};
use crate::hash::{self, ContentHash};
use crate::remote::{RResult, Remote, RemoteError, RemoteMeta, Storage, UploadStatus};
use crate::util::LocalMeta;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

pub struct JobSpec {
    pub id: u64,
    pub abs: PathBuf,
    /// Snapshot of the entry when the job was scheduled.
    pub entry: Entry,
    /// Where the item belongs in the cloud.
    pub name: String,
    pub parent_id: String,
    /// Only fix name/parent (a cloud-only entry that was renamed locally).
    pub placement_only: bool,
    /// Check that the cloud holds this exact content so the local copy can be freed.
    pub free: bool,
    pub chunk: usize,
}

#[derive(Debug)]
pub struct JobDone {
    pub drive_id: String,
    pub cloud_name: String,
    pub cloud_parent: String,
    /// Files: the hash the provider reports for the stored content.
    pub hash: Option<ContentHash>,
    pub size: Option<i64>,
    /// Files: local metadata of the content that was hashed/uploaded.
    pub local: Option<LocalMeta>,
    /// Files: the provider's hash equals the one we computed before uploading.
    pub content_ok: bool,
    pub uploaded_bytes: u64,
}

#[derive(Debug)]
pub enum JobOutcome {
    Done(JobDone),
    /// Free job: the cloud has identical content; `local` is the file as it was checked.
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
    /// The cloud file exists but holds different content (edited elsewhere).
    CloudDiffers,
    /// The cloud file was deleted or trashed.
    CloudGone,
}

/// Messages a running job sends to the engine.
pub enum JobEvent {
    Session { id: u64, url: String, hash: ContentHash },
    /// Account storage, and who is signed in.
    Quota(Storage),
    /// Bytes of a file upload the cloud has confirmed so far.
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

fn done_meta(spec: &JobSpec, drive_id: String) -> JobDone {
    JobDone {
        drive_id,
        cloud_name: spec.name.clone(),
        cloud_parent: spec.parent_id.clone(),
        hash: spec.entry.hash.clone(),
        size: spec.entry.cloud_size,
        local: None,
        content_ok: true,
        uploaded_bytes: 0,
    }
}

/// Renames/moves `id` so it is called `spec.name` inside `spec.parent_id`.
async fn place(remote: &Remote, id: &str, spec: &JobSpec) -> RResult<()> {
    let e = &spec.entry;
    let rename = e.cloud_name.as_deref() != Some(spec.name.as_str());
    let moved = e.cloud_parent.as_deref() != Some(spec.parent_id.as_str());
    if !rename && !moved {
        return Ok(());
    }
    let old_parent = match (&e.cloud_parent, moved) {
        (_, false) => None,
        (Some(p), true) => Some(p.clone()),
        (None, true) => remote.get(id).await?.parents.first().cloned(),
    };
    remote
        .move_rename(
            id,
            rename.then_some(spec.name.as_str()),
            old_parent
                .as_deref()
                .map(|old| (spec.parent_id.as_str(), old)),
        )
        .await?;
    Ok(())
}

async fn placement_job(spec: &JobSpec, remote: &Remote) -> JobOutcome {
    let Some(id) = spec.entry.drive_id.clone() else {
        return JobOutcome::Failed(RemoteError::Permanent("nothing in Drive to move".into()));
    };
    match place(remote, &id, spec).await {
        Ok(()) => JobOutcome::Done(done_meta(spec, id)),
        Err(e) => JobOutcome::Failed(e),
    }
}

/// Re-hashes the local file and asks the cloud for its hash; never deletes anything itself.
async fn free_job(spec: &JobSpec, remote: &Remote) -> JobOutcome {
    let (Some(id), Some(expected)) = (spec.entry.drive_id.clone(), spec.entry.hash.clone()) else {
        return JobOutcome::FreeSkip(FreeSkip::CloudGone);
    };
    let (abs, kind) = (spec.abs.clone(), expected.kind);
    let hashed = tokio::task::spawn_blocking(move || hash::hash_file(kind, &abs)).await;
    let (hash, local) = match hashed {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return io_failure(e, "read the file"),
        Err(e) => return JobOutcome::Failed(RemoteError::Permanent(format!("hash task failed: {e}"))),
    };
    if hash != expected {
        return JobOutcome::FreeSkip(FreeSkip::LocalChanged);
    }
    match remote.get(&id).await {
        Ok(m) if !m.trashed && m.hash.as_ref() == Some(&hash) && m.size == Some(local.size as u64) => {
            JobOutcome::FreeReady(local)
        }
        Ok(m) if !m.trashed => JobOutcome::FreeSkip(FreeSkip::CloudDiffers),
        Ok(_) | Err(RemoteError::NotFound) => JobOutcome::FreeSkip(FreeSkip::CloudGone),
        Err(e) => JobOutcome::Failed(e),
    }
}

async fn dir_job(spec: &JobSpec, remote: &Remote) -> JobOutcome {
    if let Some(id) = spec.entry.drive_id.clone() {
        match place(remote, &id, spec).await {
            Ok(()) => return JobOutcome::Done(done_meta(spec, id)),
            // Folder deleted in the cloud: make it again below.
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
        Ok(id) => JobOutcome::Done(done_meta(spec, id)),
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
    let (abs, kind) = (spec.abs.clone(), remote.capabilities().hash);
    let hashed = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        let mut f = std::fs::File::open(&abs)?;
        let md = f.metadata()?;
        if !md.is_file() {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not a file"));
        }
        let meta = LocalMeta::of_file(&f)?;
        let hash = hash::hash_reader(kind, &mut f)?;
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
        let cloud_hash = meta.and_then(|m| m.hash.clone()).unwrap_or(hash.clone());
        JobDone {
            drive_id: id,
            cloud_name: spec.name.clone(),
            cloud_parent: spec.parent_id.clone(),
            content_ok: cloud_hash == hash,
            hash: Some(cloud_hash),
            size: Some(meta.and_then(|m| m.size).unwrap_or(size) as i64),
            local: Some(local),
            uploaded_bytes: uploaded,
        }
    };

    // Same content already in the cloud: at most a rename/move.
    if let Some(id) = drive_id.clone()
        && e.hash.as_ref() == Some(&hash)
    {
        match place(remote, &id, spec).await {
            Ok(()) => return JobOutcome::Done(done(id, None, 0)),
            Err(RemoteError::NotFound) => drive_id = None,
            Err(err) => return JobOutcome::Failed(err),
        }
    }

    // Replacing content: make sure the cloud file still exists and isn't trashed.
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
        match remote.find_child(&spec.name, &spec.parent_id, false).await {
            Ok(found) => {
                if let Some(m) = found.iter().find(|m| m.hash.as_ref() == Some(&hash)) {
                    return JobOutcome::Done(done(m.id.clone(), Some(m), 0));
                }
            }
            Err(err) => return JobOutcome::Failed(err),
        }
    }

    let mime = mime_guess::from_path(&spec.name)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    let mut session = e
        .upload_url
        .clone()
        .filter(|_| e.upload_hash.as_ref() == Some(&hash));
    let mut file = Some(file);

    for _ in 0..3 {
        let mut offset = 0u64;
        let url = match session.take() {
            Some(url) => match remote.query_upload(&url, size).await {
                Ok(UploadStatus::Next(n)) => {
                    offset = n;
                    url
                }
                Ok(UploadStatus::Done(m)) => return finish(remote, m, &done).await,
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
                            hash: hash.clone(),
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
            Ok(m) => return finish(remote, m, &done).await,
            // Session expired mid-way: start a fresh one.
            Err(RemoteError::NotFound) => continue,
            Err(err) => return JobOutcome::Failed(err),
        }
    }
    JobOutcome::Failed(RemoteError::Transient("upload session keeps expiring".into()))
}

async fn finish(
    remote: &Remote,
    mut meta: RemoteMeta,
    done: &impl Fn(String, Option<&RemoteMeta>, u64) -> JobDone,
) -> JobOutcome {
    if meta.hash.is_none() {
        match remote.get(&meta.id).await {
            Ok(m) => meta = m,
            Err(err) => return JobOutcome::Failed(err),
        }
    }
    let uploaded = meta.size.unwrap_or(0);
    JobOutcome::Done(done(meta.id.clone(), Some(&meta), uploaded))
}

/// Uploads from `offset` to the end; returns the cloud's metadata for the finished file.
async fn send_chunks(
    remote: &Remote,
    url: &str,
    file: &mut Option<std::fs::File>,
    mut offset: u64,
    size: u64,
    chunk: usize,
    report: &impl Fn(u64),
) -> RResult<RemoteMeta> {
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

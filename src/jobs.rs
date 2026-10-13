//! One unit of Drive work for one entry. Jobs never touch the database: they report
//! back to the engine, which is the single writer and knows where the entry is now.
//!
//! Every step is safe to interrupt: uploads go through resumable sessions whose URL
//! the engine persists, and creation first looks for an item it may already have
//! made (crash between "created in Drive" and "recorded locally").

use crate::db::{Entry, Kind};
use crate::drive::{DResult, Drive, DriveError, FileMeta, UploadStatus};
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
    /// Where the item belongs in Drive.
    pub name: String,
    pub parent_id: String,
    /// Only fix name/parent (a cloud-only entry that was renamed locally).
    pub placement_only: bool,
    /// Check that Drive holds this exact content so the local copy can be freed.
    pub free: bool,
    pub chunk: usize,
}

#[derive(Debug)]
pub struct JobDone {
    pub drive_id: String,
    pub cloud_name: String,
    pub cloud_parent: String,
    /// Files: md5 Drive reports for the stored content.
    pub md5: Option<String>,
    pub size: Option<i64>,
    /// Files: local metadata of the content that was hashed/uploaded.
    pub local: Option<LocalMeta>,
    /// Files: Drive's md5 equals the md5 we computed before uploading.
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
    Failed(DriveError),
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
    Session { id: u64, url: String, md5: String },
    /// Account storage: (limit, usage, usage in Drive).
    Quota(Option<u64>, u64, u64),
    /// Bytes of a file upload Drive has confirmed so far.
    Progress { id: u64, sent: u64, total: u64 },
    Finished { id: u64, outcome: JobOutcome },
}

pub async fn run(spec: JobSpec, drive: Arc<Drive>, tx: UnboundedSender<JobEvent>) {
    let id = spec.id;
    let outcome = if spec.free {
        free_job(&spec, &drive).await
    } else if spec.placement_only {
        placement_job(&spec, &drive).await
    } else {
        match spec.entry.kind {
            Kind::Dir => dir_job(&spec, &drive).await,
            Kind::File => file_job(&spec, &drive, &tx).await,
        }
    };
    let _ = tx.send(JobEvent::Finished { id, outcome });
}

fn done_meta(spec: &JobSpec, drive_id: String) -> JobDone {
    JobDone {
        drive_id,
        cloud_name: spec.name.clone(),
        cloud_parent: spec.parent_id.clone(),
        md5: spec.entry.md5.clone(),
        size: spec.entry.cloud_size,
        local: None,
        content_ok: true,
        uploaded_bytes: 0,
    }
}

/// Renames/moves `id` so it is called `spec.name` inside `spec.parent_id`.
async fn place(drive: &Drive, id: &str, spec: &JobSpec) -> DResult<()> {
    let e = &spec.entry;
    let rename = e.cloud_name.as_deref() != Some(spec.name.as_str());
    let moved = e.cloud_parent.as_deref() != Some(spec.parent_id.as_str());
    if !rename && !moved {
        return Ok(());
    }
    let old_parent = match (&e.cloud_parent, moved) {
        (_, false) => None,
        (Some(p), true) => Some(p.clone()),
        (None, true) => drive.get(id).await?.parents.first().cloned(),
    };
    drive
        .update_meta(
            id,
            rename.then_some(spec.name.as_str()),
            old_parent
                .as_deref()
                .map(|old| (spec.parent_id.as_str(), old)),
        )
        .await?;
    Ok(())
}

async fn placement_job(spec: &JobSpec, drive: &Drive) -> JobOutcome {
    let Some(id) = spec.entry.drive_id.clone() else {
        return JobOutcome::Failed(DriveError::Permanent("nothing in Drive to move".into()));
    };
    match place(drive, &id, spec).await {
        Ok(()) => JobOutcome::Done(done_meta(spec, id)),
        Err(e) => JobOutcome::Failed(e),
    }
}

/// Re-hashes the local file and asks Drive for its checksum; never deletes anything itself.
async fn free_job(spec: &JobSpec, drive: &Drive) -> JobOutcome {
    let (Some(id), Some(expected)) = (spec.entry.drive_id.clone(), spec.entry.md5.clone()) else {
        return JobOutcome::FreeSkip(FreeSkip::CloudGone);
    };
    let abs = spec.abs.clone();
    let hashed = tokio::task::spawn_blocking(move || crate::util::md5_file(&abs)).await;
    let (md5, local) = match hashed {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return io_failure(e, "read the file"),
        Err(e) => return JobOutcome::Failed(DriveError::Permanent(format!("hash task failed: {e}"))),
    };
    if md5 != expected {
        return JobOutcome::FreeSkip(FreeSkip::LocalChanged);
    }
    match drive.get(&id).await {
        Ok(m) if !m.trashed
            && m.md5_checksum.as_deref() == Some(md5.as_str())
            && m.size_bytes() == Some(local.size as u64) =>
        {
            JobOutcome::FreeReady(local)
        }
        Ok(m) if !m.trashed => JobOutcome::FreeSkip(FreeSkip::CloudDiffers),
        Ok(_) | Err(DriveError::NotFound) => JobOutcome::FreeSkip(FreeSkip::CloudGone),
        Err(e) => JobOutcome::Failed(e),
    }
}

async fn dir_job(spec: &JobSpec, drive: &Drive) -> JobOutcome {
    if let Some(id) = spec.entry.drive_id.clone() {
        match place(drive, &id, spec).await {
            Ok(()) => return JobOutcome::Done(done_meta(spec, id)),
            // Folder deleted in Drive: make it again below.
            Err(DriveError::NotFound) => {}
            Err(e) => return JobOutcome::Failed(e),
        }
    }
    let result = async {
        if let Some(existing) = drive
            .find_child(&spec.name, &spec.parent_id, true)
            .await?
            .into_iter()
            .next()
        {
            return Ok(existing.id);
        }
        Ok(drive.create_folder(&spec.name, &spec.parent_id).await?.id)
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
        JobOutcome::Failed(DriveError::Permanent(format!("cannot {what}: {e}")))
    }
}

async fn file_job(spec: &JobSpec, drive: &Drive, tx: &UnboundedSender<JobEvent>) -> JobOutcome {
    // Hold one descriptor for hashing and uploading, so a rename or delete mid-way
    // cannot make us upload something other than what we hashed.
    let abs = spec.abs.clone();
    let hashed = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        let mut f = std::fs::File::open(&abs)?;
        let md = f.metadata()?;
        if !md.is_file() {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not a file"));
        }
        let meta = LocalMeta::of_file(&f)?;
        let md5 = crate::util::md5_reader(&mut f)?;
        Ok((f, md5, meta))
    })
    .await;
    let (file, md5, local) = match hashed {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return io_failure(e, "read the file"),
        Err(e) => return JobOutcome::Failed(DriveError::Permanent(format!("hash task failed: {e}"))),
    };
    let size = local.size as u64;
    let e = &spec.entry;
    let report = |sent: u64| {
        let _ = tx.send(JobEvent::Progress { id: spec.id, sent, total: size });
    };
    report(0);
    let mut drive_id = e.drive_id.clone();
    let done = |id: String, meta: Option<&FileMeta>, uploaded: u64| {
        let cloud_md5 = meta.and_then(|m| m.md5_checksum.clone()).unwrap_or(md5.clone());
        JobDone {
            drive_id: id,
            cloud_name: spec.name.clone(),
            cloud_parent: spec.parent_id.clone(),
            content_ok: cloud_md5 == md5,
            md5: Some(cloud_md5),
            size: Some(meta.and_then(FileMeta::size_bytes).unwrap_or(size) as i64),
            local: Some(local),
            uploaded_bytes: uploaded,
        }
    };

    // Same content already in Drive: at most a rename/move.
    if let Some(id) = drive_id.clone()
        && e.md5.as_deref() == Some(md5.as_str())
    {
        match place(drive, &id, spec).await {
            Ok(()) => return JobOutcome::Done(done(id, None, 0)),
            Err(DriveError::NotFound) => drive_id = None,
            Err(err) => return JobOutcome::Failed(err),
        }
    }

    // Replacing content: make sure the Drive file still exists and isn't trashed.
    if let Some(id) = drive_id.clone() {
        match drive.get(&id).await {
            Ok(m) if !m.trashed => {
                if e.pin_prev
                    && let Err(err) = drive.pin_head_revision(&id).await
                {
                    return JobOutcome::Failed(err);
                }
            }
            Ok(_) | Err(DriveError::NotFound) => drive_id = None,
            Err(err) => return JobOutcome::Failed(err),
        }
    }

    // New file: adopt an identical one already in the folder (crash recovery, lost database).
    if drive_id.is_none() {
        match drive.find_child(&spec.name, &spec.parent_id, false).await {
            Ok(found) => {
                if let Some(m) = found
                    .iter()
                    .find(|m| m.md5_checksum.as_deref() == Some(md5.as_str()))
                {
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
        .filter(|_| e.upload_md5.as_deref() == Some(md5.as_str()));
    let mut file = Some(file);

    for _ in 0..3 {
        let mut offset = 0u64;
        let url = match session.take() {
            Some(url) => match drive.query_upload(&url, size).await {
                Ok(UploadStatus::Next(n)) => {
                    offset = n;
                    url
                }
                Ok(UploadStatus::Done(m)) => return finish(drive, m, &done).await,
                Err(DriveError::NotFound) => continue,
                Err(err) => return JobOutcome::Failed(err),
            },
            None => {
                let started = drive
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
                            md5: md5.clone(),
                        });
                        url
                    }
                    Err(DriveError::NotFound) if drive_id.is_some() => {
                        drive_id = None;
                        continue;
                    }
                    Err(err) => return JobOutcome::Failed(err),
                }
            }
        };

        match send_chunks(drive, &url, &mut file, offset, size, spec.chunk, &report).await {
            Ok(m) => return finish(drive, m, &done).await,
            // Session expired mid-way: start a fresh one.
            Err(DriveError::NotFound) => continue,
            Err(err) => return JobOutcome::Failed(err),
        }
    }
    JobOutcome::Failed(DriveError::Transient("upload session keeps expiring".into()))
}

async fn finish(
    drive: &Drive,
    mut meta: FileMeta,
    done: &impl Fn(String, Option<&FileMeta>, u64) -> JobDone,
) -> JobOutcome {
    if meta.md5_checksum.is_none() {
        match drive.get(&meta.id).await {
            Ok(m) => meta = m,
            Err(err) => return JobOutcome::Failed(err),
        }
    }
    let uploaded = meta.size_bytes().unwrap_or(0);
    JobOutcome::Done(done(meta.id.clone(), Some(&meta), uploaded))
}

/// Uploads from `offset` to the end; returns Drive's metadata for the finished file.
async fn send_chunks(
    drive: &Drive,
    url: &str,
    file: &mut Option<std::fs::File>,
    mut offset: u64,
    size: u64,
    chunk: usize,
    report: &impl Fn(u64),
) -> DResult<FileMeta> {
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
        .map_err(|e| DriveError::Permanent(format!("read task failed: {e}")))?;
        *file = Some(f);
        let data = data.map_err(|_| DriveError::Transient("file changed while uploading".into()))?;

        match drive.put_chunk(url, bytes::Bytes::from(data), offset, size).await? {
            UploadStatus::Done(m) => {
                report(size);
                return Ok(m);
            }
            UploadStatus::Next(next) => {
                if next <= offset && want > 0 {
                    stalls += 1;
                    if stalls >= 3 {
                        return Err(DriveError::Transient("upload is not progressing".into()));
                    }
                } else {
                    stalls = 0;
                }
                offset = next;
            }
        }
    }
}

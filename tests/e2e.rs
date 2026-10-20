//! End-to-end: the real engine and CLI actions against the fake Drive.

mod common;

use clouddrop::actions;
use clouddrop::db::State;
use common::{Harness, read};
use std::time::Duration;

fn tree(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_with_folder_structure() {
    let mut h = Harness::new().await;
    h.write("top.txt", "top");
    h.write("a/b/c.txt", "deep");
    h.write("a/d.txt", "mid");
    std::fs::create_dir_all(h.path("empty")).unwrap();
    h.start();
    h.settle().await;

    assert_eq!(
        h.cloud_tree(),
        tree(&[
            ("a/", ""),
            ("a/b/", ""),
            ("a/b/c.txt", "deep"),
            ("a/d.txt", "mid"),
            ("empty/", ""),
            ("top.txt", "top"),
        ])
    );
    for p in ["top.txt", "a", "a/b", "a/b/c.txt", "a/d.txt", "empty"] {
        assert_eq!(h.state(p), Some(State::Synced), "{p}");
    }

    // Files added while running are picked up by the watcher.
    h.write("a/b/new.txt", "fresh");
    h.settle().await;
    assert_eq!(h.cloud("a/b/new.txt").unwrap().content, b"fresh");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ignores_temp_files() {
    let mut h = Harness::new().await;
    h.write("movie.mkv.part", "partial");
    h.write(".goutputstream-XYZ", "temp");
    h.write("keep.txt", "keep");
    h.start();
    h.settle().await;
    assert_eq!(h.cloud_tree(), tree(&[("keep.txt", "keep")]));
    assert!(h.entry("movie.mkv.part").is_none());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn local_delete_keeps_cloud_copy() {
    let mut h = Harness::new().await;
    h.write("photos/one.jpg", "1");
    h.write("photos/two.jpg", "2");
    h.start();
    h.settle().await;

    std::fs::remove_file(h.path("photos/one.jpg")).unwrap();
    h.wait("one.jpg cloud-only", |h| h.state("photos/one.jpg") == Some(State::CloudOnly)).await;

    std::fs::remove_dir_all(h.path("photos")).unwrap();
    h.wait("folder cloud-only", |h| {
        h.state("photos") == Some(State::CloudOnly) && h.state("photos/two.jpg") == Some(State::CloudOnly)
    })
    .await;
    // Nothing in Drive was deleted or trashed.
    assert_eq!(
        h.cloud_tree(),
        tree(&[("photos/", ""), ("photos/one.jpg", "1"), ("photos/two.jpg", "2")])
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn renames_and_moves_without_reupload() {
    let mut h = Harness::new().await;
    h.write("docs/report.pdf", "R");
    h.write("docs/sub/notes.txt", "N");
    h.write("loose.txt", "L");
    h.start();
    h.settle().await;
    let (sessions, _) = h.stats();
    let report_id = h.cloud("docs/report.pdf").unwrap().id;

    std::fs::rename(h.path("docs"), h.path("papers")).unwrap();
    std::fs::rename(h.path("loose.txt"), h.path("papers/sub/loose-moved.txt")).unwrap();
    h.settle().await;

    assert_eq!(
        h.cloud_tree(),
        tree(&[
            ("papers/", ""),
            ("papers/report.pdf", "R"),
            ("papers/sub/", ""),
            ("papers/sub/loose-moved.txt", "L"),
            ("papers/sub/notes.txt", "N"),
        ])
    );
    assert_eq!(h.cloud("papers/report.pdf").unwrap().id, report_id);
    assert_eq!(h.stats().0, sessions, "renames must not re-upload");
    assert!(h.entry("docs").is_none());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_become_new_revisions() {
    let mut h = Harness::new().await;
    h.write("draft.txt", "v1");
    h.start();
    h.settle().await;
    let before = h.cloud("draft.txt").unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;
    h.write("draft.txt", "version two");
    h.settle().await;
    let after = h.cloud("draft.txt").unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.content, b"version two");
    assert_eq!(after.revisions.len(), 2);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn editor_atomic_save_updates_in_place() {
    let mut h = Harness::new().await;
    h.write("essay.txt", "first");
    h.start();
    h.settle().await;
    let id = h.cloud("essay.txt").unwrap().id;

    // Save-by-rename: write a temp file, rename it over the original before it settles.
    h.write("essay.txt.new", "second");
    std::fs::rename(h.path("essay.txt.new"), h.path("essay.txt")).unwrap();
    h.settle().await;
    assert_eq!(h.cloud_tree(), tree(&[("essay.txt", "second")]));
    assert_eq!(h.cloud("essay.txt").unwrap().id, id);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn overwriting_rename_keeps_old_copy_in_cloud() {
    let mut h = Harness::new().await;
    h.write("x.txt", "old x");
    h.write("y.txt", "y");
    h.start();
    h.settle().await;
    let y_id = h.cloud("y.txt").unwrap().id;
    let x_id = h.cloud("x.txt").unwrap().id;

    std::fs::rename(h.path("y.txt"), h.path("x.txt")).unwrap();
    h.settle().await;

    assert_eq!(h.cloud_tree(), tree(&[("x (2).txt", "old x"), ("x.txt", "y")]));
    assert_eq!(h.cloud("x.txt").unwrap().id, y_id);
    assert_eq!(h.cloud("x (2).txt").unwrap().id, x_id);
    assert_eq!(h.state("x (2).txt"), Some(State::CloudOnly));
    assert_eq!(h.state("x.txt"), Some(State::Synced));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn new_file_over_cloud_only_pins_old_revision() {
    let mut h = Harness::new().await;
    h.write("plan.txt", "original");
    h.start();
    h.settle().await;
    std::fs::remove_file(h.path("plan.txt")).unwrap();
    h.wait("cloud-only", |h| h.state("plan.txt") == Some(State::CloudOnly)).await;

    h.write("plan.txt", "a different plan");
    h.settle().await;
    let item = h.cloud("plan.txt").unwrap();
    assert_eq!(item.content, b"a different plan");
    assert_eq!(item.revisions.len(), 2);
    assert!(item.revisions[0].keep_forever, "old content must be kept forever");
    assert!(!item.revisions[1].keep_forever);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn same_file_back_over_cloud_only_is_not_reuploaded() {
    let mut h = Harness::new().await;
    h.write("same.txt", "identical");
    h.start();
    h.settle().await;
    std::fs::remove_file(h.path("same.txt")).unwrap();
    h.wait("cloud-only", |h| h.state("same.txt") == Some(State::CloudOnly)).await;
    let (sessions, _) = h.stats();

    h.write("same.txt", "identical");
    h.settle().await;
    assert_eq!(h.state("same.txt"), Some(State::Synced));
    assert_eq!(h.stats().0, sessions);
    assert_eq!(h.cloud("same.txt").unwrap().revisions.len(), 1);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn free_up_space_then_restore() {
    let mut h = Harness::new().await;
    h.write("big/video.mp4", "lots of bytes");
    h.write("big/clip.mp4", "more bytes");
    h.start();
    h.settle().await;
    let mtime = std::fs::metadata(h.path("big/video.mp4")).unwrap().modified().unwrap();

    let ctx = h.ctx();
    let remote = h.remote();
    let report = actions::free(&ctx, &remote, &["big".into()]).await.unwrap();
    assert_eq!(report.freed_files, 2, "{report:?}");
    assert!(!h.path("big/video.mp4").exists());
    assert!(h.path("big").is_dir(), "folders stay so the structure is visible");
    h.wait("cloud-only", |h| {
        h.state("big/video.mp4") == Some(State::CloudOnly) && h.state("big/clip.mp4") == Some(State::CloudOnly)
    })
    .await;
    assert_eq!(h.cloud("big/video.mp4").unwrap().content, b"lots of bytes");
    let names: Vec<_> = actions::cloud_only_children(&ctx, "big")
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect();
    assert_eq!(names, ["big/clip.mp4", "big/video.mp4"]);

    let (sessions, _) = h.stats();
    let report = actions::restore(&ctx, &remote, &["big/video.mp4".into()]).await.unwrap();
    assert_eq!(report.files, 1);
    assert_eq!(read(&h.path("big/video.mp4")), "lots of bytes");
    assert_eq!(std::fs::metadata(h.path("big/video.mp4")).unwrap().modified().unwrap(), mtime);
    h.settle().await;
    assert_eq!(h.state("big/video.mp4"), Some(State::Synced));
    assert_eq!(h.state("big/clip.mp4"), Some(State::CloudOnly));
    assert_eq!(h.stats().0, sessions, "restored file must not be uploaded again");
    assert!(
        std::fs::read_dir(h.path("big")).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".clouddrop")),
        "no temp files left"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn free_refuses_unverified_copies() {
    let mut h = Harness::new().await;
    h.write("a.txt", "A");
    h.write("b.txt", "B");
    h.start();
    h.settle().await;
    h.stop().await;

    // a.txt edited after upload (daemon stopped, so not re-uploaded yet).
    tokio::time::sleep(Duration::from_millis(20)).await;
    h.write("a.txt", "A edited");
    // b.txt vanished from Drive behind our back.
    let b_id = h.entry("b.txt").unwrap().drive_id.unwrap();
    h.mock.lock().unwrap().items.remove(&b_id);

    let ctx = h.ctx();
    let report = actions::free(&ctx, &h.remote(), &["".into()]).await.unwrap();
    assert_eq!(report.freed_files, 0, "{report:?}");
    assert_eq!(report.changed, 1);
    assert_eq!(report.cloud_mismatch, 1);
    assert!(h.path("a.txt").exists() && h.path("b.txt").exists());

    // The daemon then re-uploads the lost one.
    h.start();
    h.settle().await;
    assert_eq!(h.cloud_tree(), tree(&[("a.txt", "A edited"), ("b.txt", "B")]));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_whole_folder_after_local_delete() {
    let mut h = Harness::new().await;
    h.write("trip/day1/a.jpg", "a");
    h.write("trip/day2/b.jpg", "b");
    h.start();
    h.settle().await;
    std::fs::remove_dir_all(h.path("trip")).unwrap();
    h.wait("cloud-only", |h| h.state("trip") == Some(State::CloudOnly)).await;
    h.settle().await;

    let report = actions::restore(&h.ctx(), &h.remote(), &["trip".into()]).await.unwrap();
    assert_eq!(report.files, 2);
    assert_eq!(read(&h.path("trip/day1/a.jpg")), "a");
    assert_eq!(read(&h.path("trip/day2/b.jpg")), "b");
    h.settle().await;
    for p in ["trip", "trip/day1", "trip/day2", "trip/day1/a.jpg", "trip/day2/b.jpg"] {
        assert_eq!(h.state(p), Some(State::Synced), "{p}");
    }
    assert_eq!(h.stats().1, 4, "no duplicate folders: CloudDrop, trip, day1, day2");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_chunk_upload_survives_errors_and_restart() {
    let mut h = Harness::new().await;
    let big: String = (0..900_000).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    h.write("big.bin", &big);
    // Fail the third chunk: the job backs off with its session saved.
    h.mock.lock().unwrap().fail_at.insert(3, 503);
    h.start();
    h.wait("failure recorded", |h| h.entry("big.bin").is_some_and(|e| e.attempts >= 1)).await;
    let e = h.entry("big.bin").unwrap();
    assert!(e.upload_url.is_some(), "session kept for resume");
    h.stop().await;

    // Restart: resumes the same session instead of starting over.
    h.start();
    h.wait_for("upload to finish", Duration::from_secs(40), |h| h.state("big.bin") == Some(State::Synced))
        .await;
    assert_eq!(h.cloud("big.bin").unwrap().content, big.as_bytes());
    assert_eq!(h.stats().0, 1, "one session for the whole file");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn moves_while_stopped_are_detected_by_inode() {
    let mut h = Harness::new().await;
    h.write("inbox/scan.pdf", "S");
    h.write("inbox/memo.txt", "M");
    h.start();
    h.settle().await;
    h.stop().await;
    let (sessions, _) = h.stats();

    std::fs::rename(h.path("inbox"), h.path("archive")).unwrap();
    std::fs::rename(h.path("archive/memo.txt"), h.path("memo-2026.txt")).unwrap();

    h.start();
    h.settle().await;
    assert_eq!(
        h.cloud_tree(),
        tree(&[("archive/", ""), ("archive/scan.pdf", "S"), ("memo-2026.txt", "M")])
    );
    assert_eq!(h.stats().0, sessions);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lost_database_adopts_existing_drive_items() {
    let mut h = Harness::new().await;
    h.write("keep/one.txt", "1");
    h.write("two.txt", "2");
    h.start();
    h.settle().await;
    h.stop().await;
    let before = h.stats();

    for f in ["state.db", "state.db-wal", "state.db-shm"] {
        let _ = std::fs::remove_file(h.data.join(f));
    }
    h.reopen_db();
    h.start();
    h.settle().await;
    assert_eq!(h.stats(), before, "nothing uploaded or created again");
    assert_eq!(h.cloud_tree(), tree(&[("keep/", ""), ("keep/one.txt", "1"), ("two.txt", "2")]));
    assert_eq!(h.state("keep/one.txt"), Some(State::Synced));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_access_token_is_refreshed() {
    let mut h = Harness::new().await;
    h.write("one.txt", "1");
    h.start();
    h.settle().await;
    let refreshes = h.mock.lock().unwrap().refreshes;
    h.mock.lock().unwrap().invalidate_tokens();

    h.write("two.txt", "2");
    h.settle().await;
    assert_eq!(h.cloud("two.txt").unwrap().content, b"2");
    assert!(h.mock.lock().unwrap().refreshes > refreshes);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_sign_in_pauses_until_login() {
    let mut h = Harness::new().await;
    clouddrop::auth::save_token(
        &h.token_path(),
        &clouddrop::auth::Token { access_token: "x".into(), refresh_token: "revoked".into(), expires_at: 0 },
    )
    .unwrap();
    h.write("wait.txt", "w");
    h.start();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(h.cloud("wait.txt").is_none());

    // `clouddrop login` writes a new token file; the daemon notices and continues.
    clouddrop::auth::save_token(
        &h.token_path(),
        &clouddrop::auth::Token { access_token: "x".into(), refresh_token: "good-refresh".into(), expires_at: 0 },
    )
    .unwrap();
    h.wait_for("upload after login", Duration::from_secs(60), |h| h.state("wait.txt") == Some(State::Synced))
        .await;
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interactive_login_flow() {
    let h = Harness::new().await;
    let token_file = h.data.join("new-token.json");
    clouddrop::auth::login(&h.cfg, &token_file, |url| {
        let url = url::Url::parse(url).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["scope"], clouddrop::auth::SCOPE);
        assert_eq!(q["code_challenge_method"], "S256");
        let redirect = format!("{}/?code=abc&state={}", q["redirect_uri"], q["state"]);
        tokio::spawn(async move {
            let body = reqwest::get(redirect).await.unwrap().text().await.unwrap();
            assert!(body.contains("signed in"));
        });
    })
    .await
    .unwrap();
    let tok: clouddrop::auth::Token = serde_json::from_str(&read(&token_file)).unwrap();
    assert_eq!(tok.refresh_token, "good-refresh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&token_file).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

fn big_content(n: usize) -> String {
    (0..n).map(|i| char::from(b'a' + (i % 26) as u8)).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_during_upload_lands_on_new_name() {
    let mut h = Harness::new().await;
    let big = big_content(1_000_000);
    h.write("raw.bin", &big);
    h.mock.lock().unwrap().put_delay_ms = 300;
    h.start();
    h.wait("upload in flight", |h| h.mock.lock().unwrap().chunk_puts >= 1).await;
    std::fs::create_dir_all(h.path("sorted")).unwrap();
    std::fs::rename(h.path("raw.bin"), h.path("sorted/final.bin")).unwrap();
    h.mock.lock().unwrap().put_delay_ms = 0;
    h.settle().await;
    assert_eq!(h.cloud_tree().iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["sorted/", "sorted/final.bin"]);
    assert_eq!(h.cloud("sorted/final.bin").unwrap().content, big.as_bytes());
    assert_eq!(h.state("sorted/final.bin"), Some(State::Synced));
    assert!(h.entry("raw.bin").is_none());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_during_upload_still_keeps_cloud_copy() {
    let mut h = Harness::new().await;
    let big = big_content(1_000_000);
    h.write("gone.bin", &big);
    h.mock.lock().unwrap().put_delay_ms = 300;
    h.start();
    h.wait("upload in flight", |h| h.mock.lock().unwrap().chunk_puts >= 1).await;
    std::fs::remove_file(h.path("gone.bin")).unwrap();
    h.mock.lock().unwrap().put_delay_ms = 0;
    h.wait("cloud-only", |h| h.state("gone.bin") == Some(State::CloudOnly)).await;
    assert_eq!(h.cloud("gone.bin").unwrap().content, big.as_bytes());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn burst_of_files_has_no_duplicates() {
    let mut h = Harness::with(|c| c.parallel_uploads = 4).await;
    h.start();
    tokio::time::sleep(Duration::from_millis(500)).await;
    for d in 0..5 {
        for f in 0..40 {
            h.write(&format!("batch/d{d}/f{f}.txt"), &format!("{d}-{f}"));
        }
    }
    h.settle().await;
    let t = h.cloud_tree();
    assert_eq!(t.len(), 1 + 5 + 200, "{t:#?}");
    assert_eq!(h.stats().1, 1 + 1 + 5, "folders created exactly once");
    assert_eq!(h.cloud("batch/d3/f17.txt").unwrap().content, b"3-17");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn odd_names_and_empty_files() {
    // '\\' is a path separator on Windows, so it can't be part of a name there.
    const ODD: &str = if cfg!(windows) { "O'Brien & 50% notes.txt" } else { "O'Brien \\ notes.txt" };
    let mut h = Harness::new().await;
    h.write(ODD, "quote");
    h.write("தமிழ்/ファイル 📁.txt", "unicode");
    h.write("empty.dat", "");
    h.start();
    h.settle().await;
    assert_eq!(
        h.cloud_tree(),
        tree(&[
            (ODD, "quote"),
            ("empty.dat", ""),
            ("தமிழ்/", ""),
            ("தமிழ்/ファイル 📁.txt", "unicode"),
        ])
    );
    // Restart: adoption lookups must match these names exactly (no duplicates).
    h.stop().await;
    for f in ["state.db", "state.db-wal", "state.db-shm"] {
        let _ = std::fs::remove_file(h.data.join(f));
    }
    h.reopen_db();
    let before = h.stats();
    h.start();
    h.settle().await;
    assert_eq!(h.stats(), before);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn file_deleted_in_drive_is_recreated_on_next_edit() {
    let mut h = Harness::new().await;
    h.write("note.txt", "one");
    h.start();
    h.settle().await;
    let id = h.cloud("note.txt").unwrap().id;
    h.mock.lock().unwrap().items.remove(&id);

    tokio::time::sleep(Duration::from_millis(20)).await;
    h.write("note.txt", "two");
    h.settle().await;
    let item = h.cloud("note.txt").unwrap();
    assert_ne!(item.id, id);
    assert_eq!(item.content, b"two");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn trashed_root_folder_pauses_without_uploading() {
    let mut h = Harness::new().await;
    h.write("a.txt", "a");
    h.start();
    h.settle().await;
    h.stop().await;
    let root = h.cloud("").unwrap().id;
    h.mock.lock().unwrap().items.get_mut(&root).unwrap().trashed = true;

    h.write("b.txt", "b");
    h.start();
    tokio::time::sleep(Duration::from_secs(3)).await;
    {
        let st = h.mock.lock().unwrap();
        assert!(st.items.values().all(|i| i.name != "b.txt"), "nothing uploaded into a trashed folder");
        assert_eq!(st.items.values().filter(|i| i.name == "CloudDrop").count(), 1, "no second root folder");
    }
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_stops_uploads_and_resume_continues_the_same_session() {
    let mut h = Harness::new().await;
    let big = big_content(1_200_000);
    h.write("long.bin", &big);
    h.mock.lock().unwrap().put_delay_ms = 200;
    h.start();
    h.wait("upload in flight", |h| h.mock.lock().unwrap().chunk_puts >= 1).await;

    let ctx = h.ctx();
    clouddrop::control::set_paused(&ctx.db, true).unwrap();
    h.wait("paused status", |h| {
        clouddrop::control::read_status(&h.data).is_some_and(|s| s.phase == clouddrop::control::Phase::Paused)
            && h.state("long.bin") == Some(State::Pending)
    })
    .await;
    let puts = h.mock.lock().unwrap().chunk_puts;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(h.mock.lock().unwrap().chunk_puts <= puts + 1, "no new chunks while paused");
    assert!(h.cloud("long.bin").is_none());

    h.mock.lock().unwrap().put_delay_ms = 0;
    clouddrop::control::set_paused(&ctx.db, false).unwrap();
    h.wait("upload after resume", |h| h.state("long.bin") == Some(State::Synced)).await;
    assert_eq!(h.cloud("long.bin").unwrap().content, big.as_bytes());
    assert_eq!(h.stats().0, 1, "resumed the saved session");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn status_file_reports_progress_and_idle() {
    use clouddrop::control::{Phase, read_status};
    let mut h = Harness::new().await;
    let big = big_content(1_000_000);
    h.write("progress.bin", &big);
    h.mock.lock().unwrap().put_delay_ms = 300;
    h.start();
    h.wait("progress reported", |h| {
        read_status(&h.data).is_some_and(|s| {
            s.phase == Phase::Uploading
                && s.uploads.len() == 1
                && s.uploads[0].path == "progress.bin"
                && s.uploads[0].total == big.len() as u64
                && s.uploads[0].sent > 0
        })
    })
    .await;
    assert!(read_status(&h.data).unwrap().is_live());
    h.mock.lock().unwrap().put_delay_ms = 0;
    h.wait("idle status", |h| {
        read_status(&h.data).is_some_and(|s| s.phase == Phase::Idle && s.uploads.is_empty() && s.waiting == 0)
    })
    .await;
    h.stop().await;
    assert!(read_status(&h.data).is_none(), "status removed on exit");
}

fn kinds(h: &Harness) -> Vec<(clouddrop::db::ActivityKind, String)> {
    let mut v: Vec<_> = h.db.recent_activity(100).unwrap().into_iter().map(|a| (a.kind, a.path)).collect();
    v.reverse();
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn activity_history_records_what_happened() {
    use clouddrop::db::ActivityKind::*;
    let mut h = Harness::new().await;
    h.write("a.txt", "a");
    h.write("b.txt", "b");
    h.start();
    h.settle().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    h.write("a.txt", "a2");
    h.settle().await;
    std::fs::rename(h.path("a.txt"), h.path("renamed.txt")).unwrap();
    h.settle().await;
    std::fs::remove_file(h.path("b.txt")).unwrap();
    h.wait("b cloud-only", |h| h.state("b.txt") == Some(State::CloudOnly)).await;
    let ctx = h.ctx();
    actions::restore(&ctx, &h.remote(), &["b.txt".into()]).await.unwrap();
    actions::free(&ctx, &h.remote(), &["renamed.txt".into()]).await.unwrap();
    h.settle().await;

    let got = kinds(&h);
    let mut uploads: Vec<_> = got.iter().filter(|(k, _)| *k == Uploaded).map(|(_, p)| p.as_str()).collect();
    uploads.sort();
    assert_eq!(uploads, ["a.txt", "b.txt"]);
    let rest: Vec<_> = got.into_iter().filter(|(k, _)| *k != Uploaded).collect();
    assert_eq!(
        rest,
        [
            (Updated, "a.txt".into()),
            (Moved, "renamed.txt".into()),
            (KeptInCloud, "b.txt".into()),
            (Restored, "b.txt".into()),
            (Freed, "renamed.txt".into()),
        ]
    );
    let moved = h.db.recent_activity(100).unwrap().into_iter().find(|a| a.kind == Moved).unwrap();
    assert_eq!(moved.detail.as_deref(), Some("a.txt"));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_free_right_after_upload_respects_keep_folders() {
    let mut h = Harness::with(|c| {
        c.auto_free_days = Some(0);
        c.keep_local = vec!["Keep".into()];
    })
    .await;
    h.write("Keep/important.txt", "stay");
    h.write("Videos/big.mp4", "movie bytes");
    h.write("loose.txt", "loose");
    h.start();
    h.wait_for("auto-freed", Duration::from_secs(40), |h| {
        h.state("Videos/big.mp4") == Some(State::CloudOnly) && h.state("loose.txt") == Some(State::CloudOnly)
    })
    .await;
    assert!(!h.path("Videos/big.mp4").exists() && !h.path("loose.txt").exists());
    assert!(h.path("Videos").is_dir(), "folders stay");
    assert_eq!(h.state("Keep/important.txt"), Some(State::Synced));
    assert!(h.path("Keep/important.txt").exists(), "kept folders are never freed");
    assert_eq!(h.cloud("Videos/big.mp4").unwrap().content, b"movie bytes");
    assert!(kinds(&h).contains(&(clouddrop::db::ActivityKind::AutoFreed, "loose.txt".into())));

    // Downloading back is respected: it stays until it changes again.
    actions::restore(&h.ctx(), &h.remote(), &["loose.txt".into()]).await.unwrap();
    h.stop().await;
    h.start();
    h.settle().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.state("loose.txt"), Some(State::Synced));
    assert!(h.path("loose.txt").exists());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_free_after_days_waits_for_old_uploads() {
    let mut h = Harness::with(|c| c.auto_free_days = Some(7)).await;
    h.write("old.txt", "old");
    h.write("new.txt", "new");
    h.start();
    h.settle().await;
    h.stop().await;
    assert!(h.path("old.txt").exists() && h.path("new.txt").exists());

    // Pretend old.txt was uploaded and last changed 8 days ago.
    let eight_days = std::time::SystemTime::now() - Duration::from_secs(8 * 24 * 3600);
    std::fs::File::options().write(true).open(h.path("old.txt")).unwrap().set_modified(eight_days).unwrap();
    let mut e = h.entry("old.txt").unwrap();
    e.set_local(Some(clouddrop::util::LocalMeta::read(&h.path("old.txt")).unwrap()));
    e.uploaded_at = Some(clouddrop::util::now_ms() - 8 * 24 * 3600 * 1000);
    h.db.put(&e).unwrap();

    h.start();
    h.wait("old freed", |h| h.state("old.txt") == Some(State::CloudOnly)).await;
    assert!(!h.path("old.txt").exists());
    assert_eq!(h.state("new.txt"), Some(State::Synced));
    assert!(h.path("new.txt").exists());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_free_never_deletes_when_drive_copy_differs() {
    let mut h = Harness::new().await;
    h.write("doc.txt", "local truth");
    h.start();
    h.settle().await;
    h.stop().await;
    let id = h.entry("doc.txt").unwrap().drive_id.unwrap();
    h.mock.lock().unwrap().items.get_mut(&id).unwrap().content = b"tampered".to_vec();

    h.cfg.auto_free_days = Some(0);
    h.start();
    // The first check finds Drive's copy edited elsewhere: no delete; the local content
    // goes up as a new version of the same file, then is verified and freed.
    h.wait("freed after re-upload", |h| h.state("doc.txt") == Some(State::CloudOnly)).await;
    let doc = h.cloud("doc.txt").unwrap_or_else(|| panic!("one doc.txt expected: {:?}", h.cloud_tree()));
    assert_eq!(doc.id, id, "same Drive file, no duplicate");
    assert_eq!(doc.content, b"local truth");
    assert!(doc.revisions.iter().rev().nth(1).is_some_and(|r| r.keep_forever), "the other edit is kept");
    actions::restore(&h.ctx(), &h.remote(), &["doc.txt".into()]).await.unwrap();
    assert_eq!(read(&h.path("doc.txt")), "local truth");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn status_reports_drive_storage() {
    let mut h = Harness::new().await;
    h.write("x.txt", "12345");
    h.start();
    h.settle().await;
    h.wait("quota", |h| clouddrop::control::read_status(&h.data).is_some_and(|s| s.quota.is_some())).await;
    let q = clouddrop::control::read_status(&h.data).unwrap().quota.unwrap();
    assert_eq!(q.limit, Some(16106127360));
    assert!(q.usage >= q.usage_in_drive);
    h.stop().await;
}

// ---- Windows placeholder behaviour, simulated with common::FakeStore ----

#[tokio::test(flavor = "multi_thread")]
async fn placeholders_stay_visible_when_freed_and_are_never_uploaded() {
    let store = std::sync::Arc::new(common::FakeStore::default());
    let mut h = Harness::with(|c| c.auto_free_days = Some(0)).await;
    h.write("a.txt", "alpha content");
    h.start_with_store(store.clone());
    h.wait("auto-freed", |h| h.state("a.txt") == Some(State::CloudOnly)).await;
    h.settle().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(h.path("a.txt").exists(), "a freed placeholder stays in the folder");
    assert_eq!(h.state("a.txt"), Some(State::CloudOnly));
    assert_eq!(h.cloud("a.txt").unwrap().content, b"alpha content");
    assert_eq!(h.stats().0, 1, "the empty placeholder is never uploaded");
    let id = h.entry("a.txt").unwrap().drive_id.unwrap();
    assert!(store.synced.lock().unwrap().iter().any(|(p, d)| p == &h.path("a.txt") && *d == id));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn explorer_free_and_open_round_trip_without_reupload() {
    let store = std::sync::Arc::new(common::FakeStore::default());
    let mut h = Harness::new().await;
    h.write("b.txt", "bravo");
    h.start_with_store(store.clone());
    h.settle().await;
    let sessions = h.stats().0;

    store.dehydrate_externally(&h.path("b.txt"));
    h.wait("freed by Explorer", |h| h.state("b.txt") == Some(State::CloudOnly)).await;
    assert!(kinds(&h).contains(&(clouddrop::db::ActivityKind::Freed, "b.txt".into())));

    store.hydrate_externally(&h.path("b.txt"), b"bravo");
    h.wait("hydrated", |h| h.state("b.txt") == Some(State::Synced)).await;
    assert_eq!(h.stats().0, sessions, "opening a placeholder doesn't upload anything");
    assert_eq!(h.entry("b.txt").unwrap().uploaded_at, None, "kept until changed");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn free_up_space_in_explorer_is_checked_and_done_by_the_engine() {
    let store = std::sync::Arc::new(common::FakeStore::default());
    let mut h = Harness::new().await;
    h.write("c.txt", "charlie");
    h.start_with_store(store.clone());
    h.settle().await;

    store.unpin_externally(&h.path("c.txt"));
    h.wait("freed", |h| h.state("c.txt") == Some(State::CloudOnly)).await;
    assert!(store.dehydrated.lock().unwrap().contains(&h.path("c.txt")));
    assert!(kinds(&h).contains(&(clouddrop::db::ActivityKind::Freed, "c.txt".into())));
    assert_eq!(h.cloud("c.txt").unwrap().content, b"charlie");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_on_this_device_downloads_a_freed_file() {
    let store = std::sync::Arc::new(common::FakeStore::default());
    let mut h = Harness::new().await;
    h.write("d.txt", "delta");
    h.start_with_store(store.clone());
    h.settle().await;
    let sessions = h.stats().0;
    store.dehydrate_externally(&h.path("d.txt"));
    h.wait("freed", |h| h.state("d.txt") == Some(State::CloudOnly)).await;

    store.pin_externally(&h.path("d.txt"));
    h.wait("downloaded", |h| h.state("d.txt") == Some(State::Synced)).await;
    assert_eq!(read(&h.path("d.txt")), "delta");
    assert_eq!(h.stats().0, sessions, "nothing uploaded");
    assert_eq!(h.entry("d.txt").unwrap().uploaded_at, None, "kept until changed");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn download_back_is_requested_from_the_running_service() {
    let store = std::sync::Arc::new(common::FakeStore::default());
    let mut h = Harness::new().await;
    h.write("e.txt", "echo");
    h.start_with_store(store.clone());
    h.settle().await;
    let sessions = h.stats().0;
    store.dehydrate_externally(&h.path("e.txt"));
    h.wait("freed", |h| h.state("e.txt") == Some(State::CloudOnly)).await;

    clouddrop::control::request_hydrate(&h.db, "e.txt").unwrap();
    h.wait("downloaded", |h| h.state("e.txt") == Some(State::Synced)).await;
    assert_eq!(read(&h.path("e.txt")), "echo");
    assert_eq!(h.stats().0, sessions, "nothing uploaded");
    assert!(clouddrop::control::take_hydrate_requests(&h.db).is_empty(), "request consumed");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pinned_items_are_not_freed_automatically() {
    let store = std::sync::Arc::new(common::FakeStore::default());
    let mut h = Harness::with(|c| c.auto_free_days = Some(0)).await;
    store.pinned.lock().unwrap().push(h.path("Pinned"));
    h.write("Pinned/keep.txt", "keep");
    h.write("free.txt", "free");
    h.start_with_store(store.clone());
    h.wait("free.txt freed", |h| h.state("free.txt") == Some(State::CloudOnly)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.state("Pinned/keep.txt"), Some(State::Synced));
    assert_eq!(read(&h.path("Pinned/keep.txt")), "keep");
    h.stop().await;
}

// ---- Other providers' rules, applied to the Drive mock (common::Harness::with_caps) ----

use clouddrop::remote::{Capabilities, HashKind};
use common::drive_caps;

/// A provider without content hashes whose versions can't be pinned (WebDAV, SFTP, MEGA).
fn no_hash_caps() -> Capabilities {
    Capabilities { hash: None, pin_versions: false, ..drive_caps() }
}

async fn no_hash_harness(tweak: impl FnOnce(&mut clouddrop::config::Config)) -> Harness {
    let h = Harness::with_caps(no_hash_caps(), tweak).await;
    h.mock.lock().unwrap().no_hashes = true;
    h
}

#[tokio::test(flavor = "multi_thread")]
async fn without_hashes_freeing_downloads_and_compares_first() {
    let mut h = no_hash_harness(|_| {}).await;
    h.write("a.txt", "alpha");
    h.write("b.txt", "bravo");
    h.start();
    h.settle().await;
    let a = h.entry("a.txt").unwrap();
    assert_eq!(a.hash_kind, Some(HashKind::Sha256), "local files hashed with SHA-256");
    assert_eq!(a.md5.as_deref(), Some("8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8"));
    h.stop().await;

    // b.txt changed in the cloud behind our back, same size: only a download can tell.
    let b_id = h.entry("b.txt").unwrap().drive_id.unwrap();
    h.mock.lock().unwrap().items.get_mut(&b_id).unwrap().content = b"BRAVO".to_vec();
    let report = h.free(&[""]).await.unwrap();
    assert_eq!((report.freed_files, report.cloud_mismatch), (1, 1), "{report:?}");
    assert_eq!(h.mock.lock().unwrap().downloads, 2, "each cloud copy downloaded once to verify");
    assert!(!h.path("a.txt").exists());
    assert_eq!(read(&h.path("b.txt")), "bravo", "never deleted on a mismatch");
    assert!(
        std::fs::read_dir(&h.root).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".clouddrop")),
        "no temporary downloads left"
    );

    // The daemon uploads the local content again, keeping the other version first.
    h.start();
    h.settle().await;
    let b = h.cloud("b.txt").unwrap();
    assert_eq!(b.id, b_id);
    assert_eq!(b.content, b"bravo");
    assert!(b.revisions[0].keep_forever, "the cloud's version is kept before it is replaced");

    // Restoring records the SHA-256 again, so the file isn't uploaded back.
    let sessions = h.stats().0;
    h.restore(&["a.txt"]).await.unwrap();
    assert_eq!(read(&h.path("a.txt")), "alpha");
    h.settle().await;
    assert_eq!(h.entry("a.txt").unwrap().content_hash(), a.content_hash());
    assert_eq!(h.stats().0, sessions);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_free_without_hashes_says_verifying() {
    use clouddrop::control::{VERIFYING, read_status};
    let mut h = no_hash_harness(|c| c.auto_free_days = Some(0)).await;
    h.mock.lock().unwrap().media_delay_ms = 1500;
    h.write("movie.mp4", "frames");
    h.start();
    h.wait("verifying status", |h| read_status(&h.data).is_some_and(|s| s.message.as_deref() == Some(VERIFYING)))
        .await;
    assert!(h.path("movie.mp4").exists(), "nothing freed before the download matched");
    h.wait("freed", |h| h.state("movie.mp4") == Some(State::CloudOnly)).await;
    assert!(!h.path("movie.mp4").exists());
    assert!(h.mock.lock().unwrap().downloads >= 1);
    h.wait("verifying cleared", |h| read_status(&h.data).is_some_and(|s| s.message.is_none())).await;
    assert!(
        std::fs::read_dir(&h.data).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".clouddrop-verify")),
        "temporary download removed"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_hashes_a_lost_database_adopts_by_download() {
    let mut h = no_hash_harness(|_| {}).await;
    h.write("keep/one.txt", "1");
    h.write("two.txt", "2");
    h.start();
    h.settle().await;
    h.stop().await;
    let before = h.stats();
    for f in ["state.db", "state.db-wal", "state.db-shm"] {
        let _ = std::fs::remove_file(h.data.join(f));
    }
    h.reopen_db();
    h.start();
    h.settle().await;
    assert_eq!(h.stats(), before, "nothing uploaded or created again");
    assert!(h.mock.lock().unwrap().downloads >= 2, "adopted after comparing downloads");
    assert_eq!(h.state("keep/one.txt"), Some(State::Synced));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn path_ids_follow_renamed_folders() {
    let mut h = Harness::with_caps(Capabilities { stable_ids: false, ..drive_caps() }, |_| {}).await;
    h.mock.lock().unwrap().path_ids = true;
    h.write("docs/report.pdf", "R");
    h.write("docs/sub/notes.txt", "N");
    h.write("loose.txt", "L");
    h.start();
    h.settle().await;
    assert_eq!(h.entry("docs/sub/notes.txt").unwrap().drive_id.as_deref(), Some("root/CloudDrop/docs/sub/notes.txt"));
    let sessions = h.stats().0;

    std::fs::rename(h.path("docs"), h.path("papers")).unwrap();
    std::fs::rename(h.path("loose.txt"), h.path("papers/sub/loose-moved.txt")).unwrap();
    h.settle().await;
    assert_eq!(
        h.cloud_tree(),
        tree(&[
            ("papers/", ""),
            ("papers/report.pdf", "R"),
            ("papers/sub/", ""),
            ("papers/sub/loose-moved.txt", "L"),
            ("papers/sub/notes.txt", "N"),
        ])
    );
    assert_eq!(h.stats().0, sessions, "renames must not re-upload");
    let notes = h.entry("papers/sub/notes.txt").unwrap();
    assert_eq!(notes.drive_id.as_deref(), Some("root/CloudDrop/papers/sub/notes.txt"), "ids beneath were rewritten");
    assert_eq!(notes.cloud_parent.as_deref(), Some("root/CloudDrop/papers/sub"));
    assert_eq!(h.state("papers/sub/notes.txt"), Some(State::Synced));

    // The new ids work: an edit replaces the same file, and freeing finds it.
    tokio::time::sleep(Duration::from_millis(20)).await;
    h.write("papers/sub/notes.txt", "N2");
    h.settle().await;
    assert_eq!(h.cloud("papers/sub/notes.txt").unwrap().revisions.len(), 2);
    let report = h.free(&["papers/report.pdf"]).await.unwrap();
    assert_eq!(report.freed_files, 1, "{report:?}");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn forbidden_characters_and_case_only_clashes() {
    let caps = Capabilities { forbidden_chars: ":?", case_insensitive: true, ..drive_caps() };
    let mut h = Harness::with_caps(caps, |_| {}).await;
    {
        let mut st = h.mock.lock().unwrap();
        st.forbidden = ":?".into();
        st.case_insensitive = true;
    }
    h.write("a:b?.txt", "colon");
    h.write("Report.txt", "upper");
    h.write("report.txt", "lower");
    h.start();
    h.settle().await;
    let mut t = h.cloud_tree();
    t.sort();
    assert_eq!(t, tree(&[("Report.txt", "upper"), ("a：b？.txt", "colon"), ("report (2).txt", "lower")]));
    assert_eq!(h.entry("a:b?.txt").unwrap().cloud_name.as_deref(), Some("a：b？.txt"));
    assert_eq!(h.entry("report.txt").unwrap().cloud_name.as_deref(), Some("report (2).txt"));
    assert!(h.idle() && h.db.errors(10).unwrap().is_empty());

    // Renames map names the same way; restarts leave the chosen names alone.
    std::fs::rename(h.path("a:b?.txt"), h.path("c:d.txt")).unwrap();
    h.settle().await;
    assert!(h.cloud("c：d.txt").is_some_and(|i| i.content == b"colon"));
    h.stop().await;
    let before = h.stats();
    for f in ["state.db", "state.db-wal", "state.db-shm"] {
        let _ = std::fs::remove_file(h.data.join(f));
    }
    h.reopen_db();
    h.start();
    h.settle().await;
    assert_eq!(h.stats(), before, "adopted by their cloud names, nothing uploaded again");
    assert_eq!(h.cloud_tree().len(), 3);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn generic_oauth_sign_in_has_no_client_secret() {
    use clouddrop::oauth::{OAuthClient, TokenFile, login};
    static APP: clouddrop::remote::OAuthApp = clouddrop::remote::OAuthApp {
        auth_url: "https://unused.example/auth",
        token_url: "https://unused.example/token",
        scopes: &["files", "offline_access"],
        extra_auth_params: &[("token_access_type", "offline")],
    };
    let h = Harness::new().await;
    let base = h.mock.lock().unwrap().base.clone();
    let client = OAuthClient {
        auth_url: format!("{base}/auth"),
        token_url: format!("{base}/token"),
        ..OAuthClient::new(&APP, "public-client")
    };
    let token_file = h.data.join("token-example.json");
    login(&client, &token_file, |url| {
        let url = url::Url::parse(url).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!((q["scope"].as_str(), q["token_access_type"].as_str()), ("files offline_access", "offline"));
        let redirect = format!("{}/?code=abc&state={}", q["redirect_uri"], q["state"]);
        tokio::spawn(async move { reqwest::get(redirect).await.unwrap() });
    })
    .await
    .unwrap();
    let tok: clouddrop::auth::Token = serde_json::from_str(&read(&token_file)).unwrap();
    assert_eq!(tok.refresh_token, "good-refresh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&token_file).unwrap().permissions().mode() & 0o777, 0o600);
    }

    // The token file refreshes with the refresh_token grant, still without a secret.
    let tokens = TokenFile::new(reqwest::Client::new(), &client, token_file.clone());
    let fresh = tokens.refresh().await.unwrap();
    assert!(!fresh.is_empty());
    let forms = h.mock.lock().unwrap().token_forms.clone();
    assert_eq!(forms.len(), 2);
    assert_eq!(forms[0]["grant_type"], "authorization_code");
    assert!(forms[0].contains_key("code_verifier"));
    assert_eq!(forms[1]["grant_type"], "refresh_token");
    assert!(forms.iter().all(|f| f["client_id"] == "public-client" && !f.contains_key("client_secret")));
}

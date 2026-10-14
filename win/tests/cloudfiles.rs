//! The File Explorer cloud-folder mechanics on a real Windows: a sync root in a
//! temporary folder, a provider serving data from memory (standing in for Drive),
//! and CloudDrop's WinStore marking, freeing and detecting placeholders.

#![cfg(windows)]

use cloud_filter::error::{CResult, CloudErrorKind};
use cloud_filter::filter::{Request, SyncFilter, info, ticket};
use cloud_filter::root::{
    HydrationType, PopulationType, ProtectionMode, SecurityId, Session, SyncRootIdBuilder,
    SyncRootInfo,
};
use cloud_filter::utility::WriteAt;
use clouddrop::store::LocalStore;
use clouddrop::winstore::WinStore;
use std::os::windows::fs::MetadataExt;

struct Memory(Vec<u8>);

impl SyncFilter for Memory {
    fn fetch_data(&self, _r: Request, ticket: ticket::FetchData, info: info::FetchData) -> CResult<()> {
        let range = info.required_file_range();
        let start = (range.start - range.start % 4096) as usize;
        let end = (range.end as usize).min(self.0.len());
        ticket
            .write_at(&self.0[start..end], start as u64)
            .map_err(|_| CloudErrorKind::Unsuccessful)
    }
    fn dehydrate(&self, _r: Request, ticket: ticket::Dehydrate, _i: info::Dehydrate) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }
    fn delete(&self, _r: Request, ticket: ticket::Delete, _i: info::Delete) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::Unsuccessful)
    }
}

#[test]
fn placeholder_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("CloudDropTest");
    std::fs::create_dir_all(&root).unwrap();
    let content: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();

    let id = SyncRootIdBuilder::new("CloudDropTest")
        .user_security_id(SecurityId::current_user().unwrap())
        .account_name(format!("test{}", std::process::id()))
        .build();
    let exe = std::env::current_exe().unwrap();
    id.register(
        SyncRootInfo::default()
            .with_display_name("CloudDrop test")
            .with_icon(format!("{},0", exe.display()))
            .with_version("1")
            .with_hydration_type(HydrationType::Full)
            .with_population_type(PopulationType::AlwaysFull)
            .with_allow_pinning(true)
            .with_protection_mode(ProtectionMode::Personal)
            .with_path(&root)
            .unwrap(),
    )
    .expect("register sync root");
    let connection = Session::new()
        .connect(&root, Memory(content.clone()))
        .expect("connect sync root");

    let result = std::panic::catch_unwind(|| {
        assert!(clouddrop::winstore::is_sync_root(&root));
        let file = root.join("a.bin");
        std::fs::write(&file, &content).unwrap();
        let store = WinStore;

        store.synced(&file, "drive-id-1", false);
        assert!(!store.is_dehydrated(&file));

        store.free(&file, "drive-id-1").expect("dehydrate");
        assert!(store.is_dehydrated(&file), "freed file is a placeholder");
        assert!(file.exists(), "and stays in the folder");
        let id = clouddrop::util::LocalMeta::read(&file).unwrap().inode;
        assert_ne!(id, 0, "placeholders have a file id without being downloaded");
        assert!(store.is_dehydrated(&file), "reading metadata didn't download it");

        let back = std::fs::read(&file).expect("read downloads it on demand");
        assert_eq!(back, content);
        assert!(!store.is_dehydrated(&file));

        store.pending(&file);
        store.synced(&file, "drive-id-1", false);
        let dir = root.join("Folder");
        std::fs::create_dir(&dir).unwrap();
        store.synced(&dir, "drive-id-2", true);
        assert!(
            clouddrop::winstore::is_placeholder_path(&dir).unwrap(),
            "folders get Explorer's in-sync mark too"
        );

        store.free(&file, "drive-id-1").expect("dehydrate");
        assert!(store.is_dehydrated(&file));
        store.hydrate(&file).expect("download back");
        assert!(!store.is_dehydrated(&file));
        assert_eq!(std::fs::read(&file).unwrap(), content);

        // Set up for the uninstall check below: one freed file, one kept.
        std::fs::write(dir.join("kept.txt"), b"kept").unwrap();
        store.synced(&dir.join("kept.txt"), "drive-id-3", false);
        store.free(&file, "drive-id-1").expect("dehydrate again");
    });

    drop(connection);
    let released = result.is_ok().then(|| {
        std::panic::catch_unwind(|| {
            // Uninstall: freed files go (they're in Drive), the rest become plain files.
            clouddrop_agent::cloud::release(&root);
            assert!(!root.join("a.bin").exists(), "freed placeholder removed");
            let kept = root.join("Folder").join("kept.txt");
            assert_eq!(std::fs::read(&kept).unwrap(), b"kept");
            for p in [kept, root.join("Folder")] {
                let attrs = std::fs::metadata(&p).unwrap().file_attributes();
                assert_eq!(attrs & 0x400, 0, "{} is no longer a placeholder", p.display());
            }
        })
    });
    let _ = id.unregister();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    if let Some(Err(panic)) = released {
        std::panic::resume_unwind(panic);
    }
}

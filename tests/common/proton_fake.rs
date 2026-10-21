//! Proton's CLI stand-in (`examples/fake_proton_drive.rs`) in a folder of its own per test.

use clouddrop::proton::ProtonConfig;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The built example; `cargo test` builds examples, a filtered run builds it here.
fn binary() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile = exe.parent().and_then(Path::parent).unwrap();
        let bin = profile.join("examples").join(format!("fake_proton_drive{}", std::env::consts::EXE_SUFFIX));
        if !bin.exists() {
            let mut cargo = std::process::Command::new(env!("CARGO"));
            cargo.args(["build", "--example", "fake_proton_drive", "--manifest-path"]);
            cargo.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
            if !cfg!(debug_assertions) {
                cargo.arg("--release");
            }
            assert!(cargo.status().unwrap().success(), "cannot build the fake proton-drive");
        }
        assert!(bin.exists(), "no fake proton-drive at {}", bin.display());
        bin
    })
}

/// A fake Proton Drive: `proton-drive` in a folder that holds its drive.
pub struct FakeProton {
    pub dir: tempfile::TempDir,
    pub cli: PathBuf,
}

impl FakeProton {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join(format!("proton-drive{}", std::env::consts::EXE_SUFFIX));
        // A link keeps the program's path (it finds its drive by it); Windows copies it.
        #[cfg(unix)]
        std::os::unix::fs::symlink(binary(), &cli).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(binary(), &cli).unwrap();
        FakeProton { dir, cli }
    }

    pub fn config(&self) -> ProtonConfig {
        ProtonConfig { cli: self.cli.to_string_lossy().into_owned() }
    }

    fn flag(&self, name: &str, on: bool) {
        let f = self.dir.path().join(name);
        if on {
            std::fs::write(f, "").unwrap();
        } else {
            let _ = std::fs::remove_file(f);
        }
    }

    pub fn signed_in(&self, yes: bool) {
        self.flag("logged-out", !yes);
    }

    pub fn full(&self, yes: bool) {
        self.flag("full", yes);
    }

    /// Every command line run so far.
    pub fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default().lines().map(String::from).collect()
    }

    /// Commands started while another was running (CloudDrop runs one at a time).
    pub fn overlaps(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("overlaps.log")).unwrap_or_default().lines().map(String::from).collect()
    }
}

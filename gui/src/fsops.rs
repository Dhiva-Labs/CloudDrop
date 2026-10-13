//! Copying files into the CloudDrop folder, and folder-path helpers.

use std::path::{Path, PathBuf};

/// "report.pdf" in `dir` → the first of "report.pdf", "report (2).pdf", … for which
/// `taken` is false.
pub fn unique_dest(dir: &Path, name: &str, taken: &dyn Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(name);
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| dir.join(clouddrop::util::numbered_name(name, n)))
        .find(|p| !taken(p))
        .expect("an unused name exists")
}

pub fn is_inside(child: &Path, parent: &Path) -> bool {
    child.starts_with(parent)
}

/// `picked` as a path relative to `sync_dir` ("/" separated), if it is a folder beneath it.
pub fn rel_in_sync(sync_dir: &Path, picked: &Path) -> Option<String> {
    let rel = picked.strip_prefix(sync_dir).ok()?;
    let rel = rel.to_str()?.trim_matches('/');
    (!rel.is_empty()).then(|| rel.to_string())
}

/// Adds a keep-local folder, dropping entries it makes redundant. None if already covered.
pub fn add_keep(list: &[String], rel: &str) -> Option<Vec<String>> {
    let rel = rel.trim_matches('/');
    if rel.is_empty() || covered(list, rel) {
        return None;
    }
    let mut out: Vec<String> = list
        .iter()
        .filter(|k| !covered(&[rel.to_string()], k.trim_matches('/')))
        .cloned()
        .collect();
    out.push(rel.to_string());
    out.sort();
    Some(out)
}

/// `rel` equals or lies beneath one of `list`.
pub fn covered(list: &[String], rel: &str) -> bool {
    list.iter().any(|k| {
        let k = k.trim_matches('/');
        rel == k || rel.starts_with(&format!("{k}/"))
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CopyReport {
    /// Top-level items copied.
    pub items: usize,
    pub files: u64,
    pub bytes: u64,
    pub problems: Vec<String>,
}

impl CopyReport {
    pub fn summary(&self, dest_name: &str) -> String {
        let what = if self.items == 1 { "1 item".to_string() } else { format!("{} items", self.items) };
        let mut s = if self.items == 0 {
            "Nothing was copied.".to_string()
        } else {
            format!("Copied {what} to {dest_name}. Uploading will start shortly.")
        };
        if let Some(first) = self.problems.first() {
            s.push_str(&format!(
                " {} couldn't be copied ({first}).",
                self.problems.len()
            ));
        }
        s
    }
}

/// Copies `sources` into `dest_dir` without ever replacing anything: a name that is taken
/// gets " (2)", " (3)", … `taken` also lets the caller reserve names that only exist in
/// Drive. Files appear atomically (copied under a hidden `.part` name, then renamed).
pub fn copy_into(dest_dir: &Path, sources: &[PathBuf], taken: &dyn Fn(&Path) -> bool) -> CopyReport {
    let mut report = CopyReport::default();
    if let Err(e) = std::fs::create_dir_all(dest_dir) {
        report.problems.push(format!("{}: {e}", dest_dir.display()));
        return report;
    }
    for src in sources {
        let Some(name) = src.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if is_inside(dest_dir, src) {
            report.problems.push(format!("{name}: a folder can't be copied into itself"));
            continue;
        }
        let dest = unique_dest(dest_dir, name, &|p| p.symlink_metadata().is_ok() || taken(p));
        match copy_tree(src, &dest, &mut report) {
            Ok(true) => report.items += 1,
            Ok(false) => {}
            Err(e) => report.problems.push(format!("{name}: {e}")),
        }
    }
    report
}

/// Returns false for things we skip (symlinks, sockets, …).
fn copy_tree(src: &Path, dest: &Path, report: &mut CopyReport) -> std::io::Result<bool> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            if let Err(e) = copy_tree(&entry.path(), &dest.join(&name), report) {
                report.problems.push(format!("{}: {e}", name.to_string_lossy()));
            }
        }
        Ok(true)
    } else if meta.is_file() {
        let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let tmp = dest.with_file_name(format!(".clouddrop-copy-{}-{name}.part", std::process::id()));
        let result = std::fs::copy(src, &tmp).and_then(|n| {
            if let Ok(modified) = meta.modified()
                && let Ok(f) = std::fs::File::options().write(true).open(&tmp)
            {
                let _ = f.set_modified(modified);
            }
            std::fs::rename(&tmp, dest)?;
            Ok(n)
        });
        match result {
            Ok(n) => {
                report.files += 1;
                report.bytes += n;
                Ok(true)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn unique_names_skip_taken() {
        let dir = Path::new("/x");
        let none = |_: &Path| false;
        assert_eq!(unique_dest(dir, "a.txt", &none), PathBuf::from("/x/a.txt"));
        let taken = |p: &Path| p == Path::new("/x/a.txt") || p == Path::new("/x/a (2).txt");
        assert_eq!(unique_dest(dir, "a.txt", &taken), PathBuf::from("/x/a (3).txt"));
        let any = |p: &Path| p.file_name().is_some_and(|n| n == "Photos");
        assert_eq!(unique_dest(dir, "Photos", &any), PathBuf::from("/x/Photos (2)"));
    }

    #[test]
    fn relative_to_sync_dir() {
        let root = Path::new("/home/me/CloudDrop");
        assert_eq!(rel_in_sync(root, Path::new("/home/me/CloudDrop/Photos/2026")), Some("Photos/2026".into()));
        assert_eq!(rel_in_sync(root, Path::new("/home/me/CloudDrop")), None);
        assert_eq!(rel_in_sync(root, Path::new("/home/me/Other")), None);
        assert_eq!(rel_in_sync(root, Path::new("/home/me/CloudDropX/a")), None);
    }

    #[test]
    fn keep_list_editing() {
        let list = vec!["Work/Old".to_string(), "Photos".to_string()];
        assert_eq!(add_keep(&list, "Photos/2026"), None);
        assert_eq!(add_keep(&list, "Photos"), None);
        assert_eq!(add_keep(&list, ""), None);
        assert_eq!(
            add_keep(&list, "Work"),
            Some(vec!["Photos".to_string(), "Work".to_string()])
        );
        assert!(!covered(&list, "Workshop/x"));
        assert!(covered(&list, "Work/Old/x"));
    }

    #[test]
    fn copies_without_overwriting() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dest = tmp.path().join("dest");
        fs::create_dir_all(src.join("album/inner")).unwrap();
        fs::write(src.join("a.txt"), "new").unwrap();
        fs::write(src.join("album/b.txt"), "b").unwrap();
        fs::write(src.join("album/inner/c.txt"), "c").unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("a.txt"), "old").unwrap();

        let none = |_: &Path| false;
        let sources = [src.join("a.txt"), src.join("album")];
        let r = copy_into(&dest, &sources, &none);
        assert_eq!(r.items, 2);
        assert_eq!(r.files, 3);
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(dest.join("a (2).txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(dest.join("album/inner/c.txt")).unwrap(), "c");

        // a second drop of the same folder gets its own copy; no temp files are left behind
        let r = copy_into(&dest, &[src.join("album")], &none);
        assert_eq!(r.items, 1);
        assert!(dest.join("album (2)/b.txt").exists());
        let leftovers: Vec<_> = fs::read_dir(&dest)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty());
        assert_eq!(r.summary("CloudDrop"), "Copied 1 item to CloudDrop. Uploading will start shortly.");
    }

    #[test]
    fn names_taken_only_in_drive_are_avoided() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("report.pdf");
        fs::write(&src, "x").unwrap();
        let dest = tmp.path().join("d");
        let in_drive = |p: &Path| p.file_name().is_some_and(|n| n == "report.pdf");
        let r = copy_into(&dest, &[src], &in_drive);
        assert_eq!(r.items, 1);
        assert!(dest.join("report (2).pdf").exists());
        assert!(!dest.join("report.pdf").exists());
    }

    #[test]
    fn refuses_to_copy_a_folder_into_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("f");
        fs::create_dir_all(folder.join("sub")).unwrap();
        let r = copy_into(&folder.join("sub"), std::slice::from_ref(&folder), &|_: &Path| false);
        assert_eq!(r.items, 0);
        assert_eq!(r.problems.len(), 1);
    }
}

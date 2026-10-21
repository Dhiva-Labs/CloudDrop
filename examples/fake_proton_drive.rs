//! A stand-in for Proton's `proton-drive` CLI, for CloudDrop's tests and for trying the
//! Proton Drive provider without an account: the `filesystem` commands CloudDrop runs, with
//! the same arguments, paths and JSON as the real CLI (see `src/proton.rs`), over a folder.
//!
//! It keeps its drive in the folder it is run from by path (a symlink or copy of this program
//! named `proton-drive` in an empty folder), or in `FAKE_PROTON_DRIVE_DIR`:
//!
//!   cargo build --example fake_proton_drive
//!   mkdir /tmp/pd && ln -s "$PWD/target/debug/examples/fake_proton_drive" /tmp/pd/proton-drive
//!   clouddrop setup --provider proton --set cli=/tmp/pd/proton-drive
//!
//! Files there steer it: `logged-out` (every command asks to sign in; `auth login` removes it),
//! `full` (uploads fail for lack of storage). `calls.log` records each command, and
//! `overlaps.log` any command started while another was running.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const VOLUME: &str = "VVVVVVVVVVVVVVVVVVVVVV";
const TIME: &str = "2026-10-21T00:00:00.000Z";

#[derive(Default, Serialize, Deserialize)]
struct State {
    next: u64,
    root: String,
    nodes: BTreeMap<String, Node>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Node {
    name: String,
    parent: Option<String>,
    folder: bool,
    trashed: bool,
    revision: u64,
    size: u64,
    sha1: String,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = match std::env::var_os("FAKE_PROTON_DRIVE_DIR") {
        Some(d) => PathBuf::from(d),
        None => Path::new(&args[0]).parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    if !dir.is_dir() {
        eprintln!("fake proton-drive: run it by its path in a folder, or set FAKE_PROTON_DRIVE_DIR");
        std::process::exit(2);
    }
    let args = &args[1..];
    log(&dir.join("calls.log"), &args.join(" "));
    let busy = dir.join("busy");
    if fs::OpenOptions::new().write(true).create_new(true).open(&busy).is_err() {
        log(&dir.join("overlaps.log"), &args.join(" "));
    }
    let result = run(&dir, args);
    let _ = fs::remove_file(&busy);
    match result {
        Ok(out) => print!("{out}"),
        Err((out, err)) => {
            print!("{out}");
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

fn log(file: &Path, line: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = writeln!(f, "{line}");
    }
}

/// Stdout, or (stdout, the message for stderr).
type Outcome = Result<String, (String, String)>;

fn fail(msg: impl Into<String>) -> (String, String) {
    (String::new(), msg.into())
}

fn run(dir: &Path, args: &[String]) -> Outcome {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["auth", "login", ..] => {
            let _ = fs::remove_file(dir.join("logged-out"));
            return Ok("Logged in.\n".into());
        }
        ["auth", "logout", ..] => {
            let _ = fs::write(dir.join("logged-out"), "");
            return Ok("Logged out.\n".into());
        }
        ["filesystem", _, ..] => {}
        _ => return Err(fail(format!("Command not found: {}", words.join(" ")))),
    }
    if dir.join("logged-out").exists() {
        return Err(fail("You need to login first"));
    }
    let command = words[1];
    let allowed: &[&str] = match command {
        "info" | "list" | "create-folder" | "rename" | "move" => &["json", "type"],
        "copy" => &["json", "name"],
        "upload" | "download" => &["json", "file-conflict-strategy", "folder-conflict-strategy", "skip-thumbnails"],
        _ => return Err(fail(format!("Command not found: filesystem {command}"))),
    };
    let (options, args) = parse(&words[2..], allowed).map_err(fail)?;
    if !options.contains_key("json") {
        return Err(fail("fake proton-drive only answers in JSON (--json)"));
    }
    let blobs = dir.join("blobs");
    fs::create_dir_all(&blobs).map_err(|e| fail(e.to_string()))?;
    let state_file = dir.join("state.json");
    let mut st: State = fs::read(&state_file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if st.root.is_empty() {
        st.root = st.add(Node::folder("root", None));
    }
    let n = |want: usize| -> Result<(), (String, String)> {
        if args.len() == want { Ok(()) } else { Err(fail(format!("Expected {want} arguments, got {}", args.len()))) }
    };
    let out = match command {
        "info" => {
            n(1)?;
            let uid = st.resolve(&args[0]).map_err(fail)?;
            format!("{}\n", st.json(&uid))
        }
        "list" => {
            n(1)?;
            let uid = st.resolve(&args[0]).map_err(fail)?;
            let items: Vec<String> = st.children(&uid).iter().map(|c| st.json(c).to_string()).collect();
            format!("[\n{}\n]\n", items.join(",\n"))
        }
        "create-folder" => {
            n(2)?;
            let parent = st.resolve(&args[0]).map_err(fail)?;
            if st.child(&parent, &args[1]).is_some() {
                return Err(fail("A file or folder with that name already exists"));
            }
            let uid = st.add(Node::folder(&args[1], Some(parent)));
            format!("{}\n", st.json(&uid))
        }
        "rename" => {
            n(2)?;
            let uid = st.resolve(&args[0]).map_err(fail)?;
            let parent = st.nodes[&uid].parent.clone().unwrap_or_default();
            if st.child(&parent, &args[1]).is_some_and(|other| other != uid) {
                return Err(fail("A file or folder with that name already exists"));
            }
            st.nodes.get_mut(&uid).unwrap().name = args[1].clone();
            format!("{}\n", st.json(&uid))
        }
        "move" | "copy" => {
            if args.len() < 2 {
                return Err(fail("Expected 2 arguments"));
            }
            let target = st.resolve(args.last().unwrap()).map_err(fail)?;
            let mut results = Vec::new();
            for source in &args[..args.len() - 1] {
                let uid = st.resolve(source).map_err(fail)?;
                let name = options.get("name").filter(|n| !n.is_empty()).cloned().unwrap_or(st.nodes[&uid].name.clone());
                if st.child(&target, &name).is_some() {
                    results.push(json!({"uid": uid, "ok": false, "error": {}}));
                } else if command == "move" {
                    st.nodes.get_mut(&uid).unwrap().parent = Some(target.clone());
                    results.push(json!({"uid": uid, "ok": true}));
                } else {
                    let mut copy = st.nodes[&uid].clone();
                    (copy.name, copy.parent) = (name, Some(target.clone()));
                    let new = st.add(copy);
                    let _ = fs::copy(blobs.join(&uid), blobs.join(&new));
                    results.push(json!({"uid": uid, "newUid": new, "ok": true}));
                }
            }
            array(&results)
        }
        "upload" => {
            if args.len() < 2 {
                return Err(fail("At least one local source and a remote parent path are required"));
            }
            let parent = st.resolve(args.last().unwrap()).map_err(fail)?;
            let strategy = options.get("file-conflict-strategy").map_or("", String::as_str);
            let (mut uploaded, mut skipped) = (0, 0);
            for local in &args[..args.len() - 1] {
                let data = fs::read(local).map_err(|e| fail(format!("{local}: {e}")))?;
                let name = Path::new(local).file_name().unwrap().to_string_lossy().into_owned();
                let sha1 = hex::encode(Sha1::digest(&data));
                if dir.join("full").exists() {
                    let summary = json!({"uploaded": 0, "failed": 1,
                        "failures": [{"name": name, "error": "Insufficient storage quota"}]});
                    return Err((format!("{summary}\n"), "1 item(s) failed to upload".into()));
                }
                let uid = match st.child(&parent, &name) {
                    Some(old) if st.nodes[&old].folder => {
                        return Err(fail(format!("Name conflict on \"{name}\" (folder) already exists")));
                    }
                    Some(old) if st.nodes[&old].sha1 == sha1 => {
                        skipped += 1;
                        continue;
                    }
                    Some(old) => match strategy {
                        "create-new-revision" => old,
                        "skip" => {
                            skipped += 1;
                            continue;
                        }
                        _ => return Err(fail(format!("Name conflict on \"{name}\" (file) already exists"))),
                    },
                    None => st.add(Node::file(&name, &parent)),
                };
                let node = st.nodes.get_mut(&uid).unwrap();
                (node.revision, node.size, node.sha1) = (node.revision + 1, data.len() as u64, sha1);
                fs::write(blobs.join(&uid), &data).map_err(|e| fail(e.to_string()))?;
                uploaded += 1;
            }
            format!("{}\n", json!({"uploaded": uploaded, "skipped": skipped, "failed": 0}))
        }
        "download" => {
            if args.len() < 2 {
                return Err(fail("At least one remote path and a local folder are required"));
            }
            let local = Path::new(args.last().unwrap());
            fs::create_dir_all(local).map_err(|e| fail(e.to_string()))?;
            for remote in &args[..args.len() - 1] {
                let uid = st.resolve(remote).map_err(fail)?;
                let node = &st.nodes[&uid];
                let to = local.join(&node.name);
                if to.exists() {
                    return Err(fail(format!("Name conflict on \"{}\" (file) already exists", node.name)));
                }
                fs::copy(blobs.join(&uid), &to).map_err(|e| fail(e.to_string()))?;
            }
            format!("{}\n", json!({"downloaded": args.len() - 1, "failed": 0}))
        }
        _ => unreachable!(),
    };
    fs::write(&state_file, serde_json::to_vec(&st).unwrap()).map_err(|e| fail(e.to_string()))?;
    Ok(out)
}

/// `--key=value` and `--flag` options up to `--`, then the arguments.
fn parse(words: &[&str], allowed: &[&str]) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let mut options = BTreeMap::new();
    let mut rest = words.iter();
    for w in rest.by_ref() {
        if *w == "--" {
            break;
        }
        let Some(opt) = w.strip_prefix("--") else {
            return Err(format!("fake proton-drive wants every argument after `--`, got {w:?}"));
        };
        let (key, value) = opt.split_once('=').unwrap_or((opt, ""));
        if !allowed.contains(&key) {
            return Err(format!("Unknown option '--{key}'"));
        }
        options.insert(key.to_string(), value.to_string());
    }
    Ok((options, rest.map(|s| s.to_string()).collect()))
}

/// The streaming JSON array the CLI prints.
fn array(items: &[Value]) -> String {
    let items: Vec<String> = items.iter().map(Value::to_string).collect();
    format!("[\n{}\n]\n", items.join(",\n"))
}

fn is_uid(s: &str) -> bool {
    let part = |p: &str| {
        (p.len() == 22 || (88..=108).contains(&p.len()))
            && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"=_-".contains(&b))
    };
    s.split_once('~').is_some_and(|(a, b)| part(a) && part(b))
}

/// Path segments; `\/` is a `/` inside a name.
fn segments(path: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'/') => out.last_mut().unwrap().push(chars.next().unwrap()),
            '/' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

impl Node {
    fn folder(name: &str, parent: Option<String>) -> Node {
        Node { name: name.into(), parent, folder: true, trashed: false, revision: 0, size: 0, sha1: String::new() }
    }

    fn file(name: &str, parent: &str) -> Node {
        Node { folder: false, ..Node::folder(name, Some(parent.into())) }
    }
}

impl State {
    fn add(&mut self, node: Node) -> String {
        self.next += 1;
        let uid = format!("{VOLUME}~{:0>22}", self.next);
        self.nodes.insert(uid.clone(), node);
        uid
    }

    fn children(&self, parent: &str) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.parent.as_deref() == Some(parent) && !n.trashed)
            .map(|(uid, _)| uid.clone())
            .collect()
    }

    fn child(&self, parent: &str, name: &str) -> Option<String> {
        self.children(parent).into_iter().find(|c| self.nodes[c].name == name)
    }

    fn resolve(&self, path: &str) -> Result<String, String> {
        let segs = segments(path);
        if segs.len() < 2 || !segs[0].is_empty() || segs[1] != "my-files" {
            return Err(format!("Path \"{path}\" not supported"));
        }
        let mut at = self.root.clone();
        for seg in segs[2..].iter().filter(|s| !s.is_empty()) {
            at = if is_uid(seg) {
                self.nodes.contains_key(seg).then(|| seg.clone()).ok_or("Node not found")?
            } else {
                self.child(&at, seg).ok_or_else(|| format!("Node not found: {seg}"))?
            };
        }
        Ok(at)
    }

    /// The node as the SDK's `NodeEntity` in JSON (absent fields left out, as JSON.stringify does).
    fn json(&self, uid: &str) -> Value {
        let n = &self.nodes[uid];
        let mut v = json!({
            "uid": uid,
            "name": {"ok": true, "value": n.name},
            "type": if n.folder { "folder" } else { "file" },
            "creationTime": TIME,
            "modificationTime": TIME,
            "isShared": false,
        });
        if let Some(p) = &n.parent {
            v["parentUid"] = json!(p);
        }
        if n.trashed {
            v["trashTime"] = json!(TIME);
        }
        if !n.folder {
            v["mediaType"] = json!("application/octet-stream");
            v["activeRevision"] = json!({
                "uid": format!("{uid}~{}", n.revision), "state": "active", "creationTime": TIME,
                "storageSize": n.size + 512, "isImported": false, "claimedSize": n.size,
                "claimedDigests": {"sha1": n.sha1, "sha1Verified": true},
            });
        }
        v
    }
}

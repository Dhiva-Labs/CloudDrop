//! A real OpenSSH server for the SFTP client's tests (rather than a fake: sshd is cheap to run
//! and catches more), plus a TCP proxy in front of it that can cut connections.
//!
//! [`Sshd::start`] runs `sshd -D -e -f` as the current user in a temp dir, with its own host
//! key, a user key in its own `authorized_keys`, and `internal-sftp`. It returns None, after
//! printing why, where sshd or ssh-keygen is missing or cannot run; set
//! `CLOUDDROP_REQUIRE_SSHD=1` to fail instead. `CLOUDDROP_SSHD` overrides sshd's path.

use clouddrop::sftp::SftpConfig;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

pub struct Sshd {
    pub dir: tempfile::TempDir,
    pub port: u16,
    pub user: String,
    /// An authorised key without a passphrase.
    pub key: PathBuf,
    /// The directory CloudDrop uploads into (`[sftp] path`).
    pub root: PathBuf,
    child: Child,
}

fn skip(why: &str) -> Option<Sshd> {
    if std::env::var_os("CLOUDDROP_REQUIRE_SSHD").is_some() {
        panic!("CLOUDDROP_REQUIRE_SSHD is set but {why}");
    }
    eprintln!("note: skipping the SFTP server tests: {why}");
    None
}

fn find(name: &str, candidates: &[&str]) -> Option<PathBuf> {
    candidates.iter().map(PathBuf::from).find(|p| p.is_file()).or_else(|| {
        let out = Command::new("sh").arg("-c").arg(format!("command -v {name}")).output().ok()?;
        let p = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
        p.is_absolute().then_some(p)
    })
}

/// Makes an ed25519 key pair at `path` (and `path.pub`).
pub fn keygen(path: &Path, passphrase: &str) {
    let ok = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", passphrase, "-C", "clouddrop-test", "-f"])
        .arg(path)
        .stdin(Stdio::null())
        .status()
        .expect("ssh-keygen runs")
        .success();
    assert!(ok, "ssh-keygen failed for {}", path.display());
}

/// `SHA256:…` of a public key file, as `ssh-keygen -l` prints it.
pub fn fingerprint_of(pub_key: &Path) -> String {
    let out = Command::new("ssh-keygen").args(["-l", "-E", "sha256", "-f"]).arg(pub_key).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    text.split_whitespace()
        .find(|t| t.starts_with("SHA256:"))
        .unwrap_or_else(|| panic!("no fingerprint in {text:?}"))
        .to_string()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Sshd {
    pub fn start() -> Option<Sshd> {
        let sshd = match std::env::var_os("CLOUDDROP_SSHD") {
            Some(p) => Some(PathBuf::from(p)),
            None => find("sshd", &["/usr/sbin/sshd", "/usr/local/sbin/sshd"]),
        };
        let Some(sshd) = sshd else {
            return skip("there is no sshd here");
        };
        if find("ssh-keygen", &[]).is_none() {
            return skip("there is no ssh-keygen here");
        }
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        keygen(&d.join("host_ed25519"), "");
        keygen(&d.join("user_key"), "");
        std::fs::copy(d.join("user_key.pub"), d.join("authorized_keys")).unwrap();
        let root = d.join("root");
        std::fs::create_dir(&root).unwrap();
        let user = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout).unwrap().trim().to_string();

        let mut last_log = String::new();
        for _ in 0..3 {
            let port = free_port();
            let config = format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_ed25519\nPidFile {d}/sshd.pid\n\
                 AuthorizedKeysFile {d}/authorized_keys\nStrictModes no\nUsePAM no\nPasswordAuthentication no\n\
                 KbdInteractiveAuthentication no\nPubkeyAuthentication yes\nSubsystem sftp internal-sftp\n\
                 LogLevel ERROR\n",
                d = d.display()
            );
            std::fs::write(d.join("sshd_config"), config).unwrap();
            let log = std::fs::File::create(d.join("sshd.log")).unwrap();
            let mut child = Command::new(&sshd)
                .arg("-D")
                .arg("-e")
                .arg("-f")
                .arg(d.join("sshd_config"))
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .expect("sshd starts");
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    return Some(Sshd { key: d.join("user_key"), dir, port, user, root, child });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
            last_log.clear();
            let _ = std::fs::File::open(d.join("sshd.log")).and_then(|mut f| f.read_to_string(&mut last_log));
        }
        skip(&format!("sshd did not start: {}", last_log.trim()))
    }

    /// The host key's fingerprint.
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.dir.path().join("host_ed25519.pub"))
    }

    /// Settings that work, through `port` (the server's own, or a proxy's).
    pub fn config(&self, port: u16) -> SftpConfig {
        SftpConfig {
            host: "127.0.0.1".into(),
            port,
            user: self.user.clone(),
            path: self.root.display().to_string(),
            key_file: self.key.display().to_string(),
            host_key: self.fingerprint(),
        }
    }

    /// A new key pair called `name`, authorised for sign-in when `authorise`.
    pub fn new_key(&self, name: &str, passphrase: &str, authorise: bool) -> PathBuf {
        let key = self.dir.path().join(name);
        keygen(&key, passphrase);
        if authorise {
            let line = std::fs::read_to_string(key.with_extension("pub")).unwrap();
            let mut f = std::fs::OpenOptions::new().append(true).open(self.dir.path().join("authorized_keys")).unwrap();
            std::io::Write::write_all(&mut f, line.as_bytes()).unwrap();
        }
        key
    }

    /// A file below the upload directory, as the server's disk has it.
    pub fn file(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Forwards TCP connections to a port, and can cut them.
pub struct Proxy {
    pub port: u16,
    /// Bytes still allowed towards the server before every connection is cut; -1 = no limit.
    budget: Arc<AtomicI64>,
    conns: Arc<std::sync::Mutex<Vec<JoinHandle<()>>>>,
    accept: JoinHandle<()>,
}

impl Proxy {
    pub async fn start(target: u16) -> Proxy {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let budget = Arc::new(AtomicI64::new(-1));
        let conns: Arc<std::sync::Mutex<Vec<JoinHandle<()>>>> = Default::default();
        let (b, c) = (budget.clone(), conns.clone());
        let accept = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let Ok(server) = tokio::net::TcpStream::connect(("127.0.0.1", target)).await else {
                    continue;
                };
                let budget = b.clone();
                c.lock().unwrap().push(tokio::spawn(forward(client, server, budget)));
            }
        });
        Proxy { port, budget, conns, accept }
    }

    /// Drops every open connection now.
    pub fn cut(&self) {
        for h in self.conns.lock().unwrap().drain(..) {
            h.abort();
        }
    }

    /// Drops every open connection once `bytes` more went towards the server.
    pub fn cut_after(&self, bytes: u64) {
        self.budget.store(bytes as i64, Ordering::SeqCst);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.accept.abort();
        self.cut();
    }
}

async fn forward(mut client: tokio::net::TcpStream, mut server: tokio::net::TcpStream, budget: Arc<AtomicI64>) {
    let (mut cr, mut cw) = client.split();
    let (mut sr, mut sw) = server.split();
    let up = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match cr.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let left = budget.load(Ordering::SeqCst);
            let pass = if left < 0 { n } else { n.min(left as usize) };
            if left >= 0 {
                budget.fetch_sub(pass as i64, Ordering::SeqCst);
            }
            if sw.write_all(&buf[..pass]).await.is_err() {
                return;
            }
            if left >= 0 && pass as i64 == left {
                // The cut: this and later connections go back to no limit.
                budget.store(-1, Ordering::SeqCst);
                return;
            }
        }
    };
    let down = tokio::io::copy(&mut sr, &mut cw);
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
}

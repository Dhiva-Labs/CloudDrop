//! End-to-end tests of the published StatusNotifierItem.
//!
//! Every test starts its own private `dbus-daemon` (no service activation, an
//! abstract socket of its own) and points `DBUS_SESSION_BUS_ADDRESS` at it before
//! anything connects, so nothing here can reach the real desktop session. A
//! mock `org.kde.StatusNotifierWatcher` plays the shell and ordinary D-Bus
//! calls inspect the item, exactly as a tray host would.
//!
//! Needs the `dbus-daemon` binary (package `dbus` / `dbus-daemon`).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use clouddrop_tray::{Tray, TrayCommand, TrayIcon, TrayState};
use futures_util::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, Proxy};

const WAIT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------- private bus

/// Tests share the process environment, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Bus {
    child: Child,
    address: String,
    _dir: tempfile::TempDir,
    _serial: MutexGuard<'static, ()>,
}

impl Bus {
    fn start() -> Bus {
        static COUNTER: AtomicU32 = AtomicU32::new(0);

        let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        // A private abstract socket: no file to clean up, no path-length limit.
        let socket = format!(
            "clouddrop-tray-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = tempfile::tempdir().expect("temp dir");
        let config = dir.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:abstract={socket}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#
            ),
        )
        .unwrap();

        let mut child = Command::new("dbus-daemon")
            .env_clear()
            .arg("--nofork")
            .arg("--print-address=1")
            .arg(format!("--config-file={}", config.display()))
            .stdout(Stdio::piped())
            .spawn()
            .expect("these tests need the dbus-daemon binary");
        let mut address = String::new();
        let read = BufReader::new(child.stdout.take().unwrap()).read_line(&mut address);
        // From here on Drop reaps the daemon, even if an assertion fails.
        let bus = Bus {
            child,
            address: address.trim().to_string(),
            _dir: dir,
            _serial: serial,
        };
        read.expect("dbus-daemon prints its address");
        // Our own abstract socket, never the desktop's /run/user/... bus.
        assert!(
            bus.address.starts_with(&format!("unix:abstract={socket}")),
            "unexpected bus address {:?}",
            bus.address
        );

        // SAFETY: SERIAL is held, so no other test is running; the others are
        // parked on the lock and nothing else reads the environment meanwhile.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.address) };
        bus
    }

    async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    /// Owns `org.kde.StatusNotifierWatcher` like the shell's AppIndicator
    /// extension does. Returns the connection (drop = watcher gone) and the
    /// item names registered with it.
    async fn start_watcher(&self) -> (Connection, Arc<Mutex<Vec<String>>>) {
        let items = Arc::new(Mutex::new(Vec::new()));
        let conn = zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .name("org.kde.StatusNotifierWatcher")
            .unwrap()
            .serve_at("/StatusNotifierWatcher", Watcher(items.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        (conn, items)
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Watcher(Arc<Mutex<Vec<String>>>);

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: &str) {
        self.0.lock().unwrap().push(service.to_string());
    }

    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

// ------------------------------------------------------------------- helpers

fn idle() -> TrayState {
    TrayState {
        icon: TrayIcon::Idle,
        status_line: "Up to date".into(),
        paused: false,
        can_pause: true,
        has_drive_folder: false,
        provider_name: "Google Drive".into(),
        web_links: true,
    }
}

type Commands = UnboundedReceiver<TrayCommand>;

async fn spawn_tray(initial: TrayState, theme: Option<PathBuf>) -> (Tray, Commands) {
    let (tx, rx) = unbounded_channel();
    let tray = Tray::spawn(initial, theme, move |c| {
        let _ = tx.send(c);
    })
    .await
    .expect("spawn succeeds when a watcher is present");
    (tray, rx)
}

async fn next_command(rx: &mut Commands) -> TrayCommand {
    timeout(WAIT, rx.recv())
        .await
        .expect("no command arrived")
        .expect("channel closed")
}

/// Properties are read fresh every time: SNI icon changes are announced with
/// signals, not PropertiesChanged, so a caching proxy would go stale.
async fn proxy<'a>(
    conn: &'a Connection,
    dest: &'a str,
    path: &'a str,
    iface: &'a str,
) -> Proxy<'a> {
    zbus::proxy::Builder::new(conn)
        .destination(dest)
        .unwrap()
        .path(path)
        .unwrap()
        .interface(iface)
        .unwrap()
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .unwrap()
}

async fn sni<'a>(conn: &'a Connection, item: &'a str) -> Proxy<'a> {
    proxy(
        conn,
        item,
        "/StatusNotifierItem",
        "org.kde.StatusNotifierItem",
    )
    .await
}

async fn dbusmenu<'a>(conn: &'a Connection, item: &'a str) -> Proxy<'a> {
    proxy(conn, item, "/MenuBar", "com.canonical.dbusmenu").await
}

/// (icon name, title, description) of the `ToolTip` property.
async fn tooltip(sni: &Proxy<'_>) -> (String, String, String) {
    type Raw = (String, Vec<(i32, i32, Vec<u8>)>, String, String);
    let (icon, _pixmaps, title, text): Raw = sni.get_property("ToolTip").await.unwrap();
    (icon, title, text)
}

type LayoutTuple = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

#[derive(Debug, PartialEq)]
struct Row {
    id: i32,
    /// The label, or "---" for a separator.
    text: String,
    enabled: bool,
}

async fn menu_rows(menu: &Proxy<'_>) -> Vec<Row> {
    let (_revision, root): (u32, LayoutTuple) = menu
        .call("GetLayout", &(0i32, -1i32, Vec::<String>::new()))
        .await
        .unwrap();
    root.2
        .into_iter()
        .map(|child| {
            let (id, props, _children): LayoutTuple = child.try_into().unwrap();
            let get = |key: &str| props.get(key).map(|v| v.try_clone().unwrap());
            let separator = get("type")
                .and_then(|v| String::try_from(v).ok())
                .as_deref()
                == Some("separator");
            let text = match get("label") {
                Some(v) if !separator => String::try_from(v).unwrap(),
                _ => "---".to_string(),
            };
            let enabled = get("enabled")
                .map(|v| bool::try_from(v).unwrap())
                .unwrap_or(true);
            Row { id, text, enabled }
        })
        .collect()
}

fn texts(rows: &[Row]) -> Vec<&str> {
    rows.iter().map(|r| r.text.as_str()).collect()
}

async fn click(menu: &Proxy<'_>, rows: &[Row], label: &str) {
    let row = rows
        .iter()
        .find(|r| r.text == label)
        .unwrap_or_else(|| panic!("no menu row {label:?}"));
    let _: () = menu
        .call("Event", &(row.id, "clicked", Value::I32(0), 0u32))
        .await
        .unwrap();
}

async fn has_owner(conn: &Connection, name: &str) -> bool {
    zbus::fdo::DBusProxy::new(conn)
        .await
        .unwrap()
        .name_has_owner(name.try_into().unwrap())
        .await
        .unwrap()
}

async fn wait_until_gone(conn: &Connection, name: &str) {
    let deadline = Instant::now() + WAIT;
    while has_owner(conn, name).await {
        assert!(Instant::now() < deadline, "{name} is still on the bus");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// --------------------------------------------------------------------- tests

#[tokio::test(flavor = "multi_thread")]
async fn registers_and_follows_state_updates() {
    let bus = Bus::start();
    let (_watcher, items) = bus.start_watcher().await;
    let client = bus.connect().await;
    let theme = PathBuf::from("/opt/clouddrop-test/icons");

    let (tray, _commands) = spawn_tray(idle(), Some(theme.clone())).await;

    // Registered with the watcher under an org.kde.StatusNotifierItem-* name.
    let item = {
        let items = items.lock().unwrap();
        assert_eq!(items.len(), 1, "exactly one item registers: {items:?}");
        items[0].clone()
    };
    assert!(item.starts_with("org.kde.StatusNotifierItem-"), "{item}");

    let sni = sni(&client, &item).await;
    let menu = dbusmenu(&client, &item).await;

    // Initial state.
    assert_eq!(sni.get_property::<String>("Id").await.unwrap(), "clouddrop");
    assert_eq!(
        sni.get_property::<String>("Category").await.unwrap(),
        "ApplicationStatus"
    );
    assert_eq!(
        sni.get_property::<String>("Title").await.unwrap(),
        "CloudDrop"
    );
    assert_eq!(
        sni.get_property::<String>("Status").await.unwrap(),
        "Active"
    );
    assert_eq!(
        sni.get_property::<String>("IconName").await.unwrap(),
        "clouddrop-idle-symbolic"
    );
    assert_eq!(
        sni.get_property::<String>("IconThemePath").await.unwrap(),
        theme.to_str().unwrap()
    );
    assert_eq!(
        tooltip(&sni).await,
        (
            "clouddrop-idle-symbolic".into(),
            "CloudDrop".into(),
            "Up to date".into()
        )
    );

    let rows = menu_rows(&menu).await;
    assert_eq!(
        texts(&rows),
        [
            "Up to date",
            "---",
            "Open CloudDrop Folder",
            "Show CloudDrop",
            "---",
            "Pause Uploads",
            "---",
            "Quit"
        ]
    );
    assert_eq!(
        rows.iter()
            .filter(|r| !r.enabled)
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>(),
        ["Up to date"],
        "only the status line is insensitive"
    );

    // Syncing, paused, with a Drive folder.
    let mut new_icon = sni.receive_signal("NewIcon").await.unwrap();
    let mut new_tip = sni.receive_signal("NewToolTip").await.unwrap();
    tray.update(TrayState {
        icon: TrayIcon::Syncing,
        status_line: "Uploading 3 files — 45%".into(),
        paused: true,
        can_pause: true,
        has_drive_folder: true,
        provider_name: "Google Drive".into(),
        web_links: true,
    })
    .await;
    timeout(WAIT, new_icon.next())
        .await
        .expect("NewIcon is announced")
        .unwrap();
    timeout(WAIT, new_tip.next())
        .await
        .expect("NewToolTip is announced")
        .unwrap();
    assert_eq!(
        sni.get_property::<String>("IconName").await.unwrap(),
        "clouddrop-syncing-symbolic"
    );
    assert_eq!(
        sni.get_property::<String>("Status").await.unwrap(),
        "Active"
    );
    assert_eq!(tooltip(&sni).await.2, "Uploading 3 files — 45%");
    assert_eq!(
        texts(&menu_rows(&menu).await),
        [
            "Uploading 3 files — 45%",
            "---",
            "Open CloudDrop Folder",
            "Open in Google Drive",
            "Show CloudDrop",
            "---",
            "Resume Uploads",
            "---",
            "Quit",
        ]
    );

    // Back before setup completes: no pause item, no Drive item.
    tray.update(TrayState {
        icon: TrayIcon::Paused,
        status_line: "Set up".into(),
        paused: false,
        can_pause: false,
        has_drive_folder: false,
        provider_name: "Google Drive".into(),
        web_links: true,
    })
    .await;
    assert_eq!(
        sni.get_property::<String>("IconName").await.unwrap(),
        "clouddrop-paused-symbolic"
    );
    assert_eq!(
        texts(&menu_rows(&menu).await),
        [
            "Set up",
            "---",
            "Open CloudDrop Folder",
            "Show CloudDrop",
            "---",
            "Quit"
        ]
    );

    // Attention asks the shell to highlight the item.
    let mut new_status = sni.receive_signal("NewStatus").await.unwrap();
    tray.update(TrayState {
        icon: TrayIcon::Attention,
        ..idle()
    })
    .await;
    timeout(WAIT, new_status.next())
        .await
        .expect("NewStatus is announced")
        .unwrap();
    assert_eq!(
        sni.get_property::<String>("Status").await.unwrap(),
        "NeedsAttention"
    );
    assert_eq!(
        sni.get_property::<String>("IconName").await.unwrap(),
        "clouddrop-attention-symbolic"
    );
    assert_eq!(
        sni.get_property::<String>("AttentionIconName")
            .await
            .unwrap(),
        "clouddrop-attention-symbolic"
    );

    // And leaving it clears the highlight again.
    tray.update(idle()).await;
    assert_eq!(
        sni.get_property::<String>("Status").await.unwrap(),
        "Active"
    );
    assert_eq!(
        sni.get_property::<String>("IconName").await.unwrap(),
        "clouddrop-idle-symbolic"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn menu_clicks_and_icon_activation_reach_the_callback() {
    let bus = Bus::start();
    let (_watcher, items) = bus.start_watcher().await;
    let client = bus.connect().await;
    let (tray, mut commands) = spawn_tray(
        TrayState {
            has_drive_folder: true,
            provider_name: "Google Drive".into(),
            web_links: true,
            ..idle()
        },
        None,
    )
    .await;
    let item = items.lock().unwrap()[0].clone();
    let sni = sni(&client, &item).await;
    let menu = dbusmenu(&client, &item).await;

    let rows = menu_rows(&menu).await;
    for (label, expected) in [
        ("Open CloudDrop Folder", TrayCommand::OpenFolder),
        ("Open in Google Drive", TrayCommand::OpenInDrive),
        ("Show CloudDrop", TrayCommand::ShowWindow),
        ("Pause Uploads", TrayCommand::TogglePause),
        ("Quit", TrayCommand::Quit),
    ] {
        click(&menu, &rows, label).await;
        assert_eq!(next_command(&mut commands).await, expected, "{label}");
    }

    // Left click on the icon.
    let _: () = sni.call("Activate", &(10i32, 20i32)).await.unwrap();
    assert_eq!(next_command(&mut commands).await, TrayCommand::ShowWindow);

    // After an update the new items (new ids) still work.
    tray.update(TrayState {
        paused: true,
        ..idle()
    })
    .await;
    let rows = menu_rows(&menu).await;
    click(&menu, &rows, "Resume Uploads").await;
    assert_eq!(next_command(&mut commands).await, TrayCommand::TogglePause);

    assert!(commands.try_recv().is_err(), "no stray commands");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_removes_the_item_from_the_bus() {
    let bus = Bus::start();
    let (_watcher, items) = bus.start_watcher().await;
    let client = bus.connect().await;
    let (tray, _commands) = spawn_tray(idle(), None).await;
    let item = items.lock().unwrap()[0].clone();
    assert!(has_owner(&client, &item).await);

    tray.shutdown().await;
    wait_until_gone(&client, &item).await;
    // Idempotent, and the icon stays gone when the handle is dropped later.
    tray.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_handle_removes_the_item_from_the_bus() {
    let bus = Bus::start();
    let (_watcher, items) = bus.start_watcher().await;
    let client = bus.connect().await;
    let (tray, _commands) = spawn_tray(idle(), None).await;
    let item = items.lock().unwrap()[0].clone();
    assert!(has_owner(&client, &item).await);

    drop(tray);
    wait_until_gone(&client, &item).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_without_a_watcher_fails_instead_of_hanging() {
    let _bus = Bus::start();
    let started = Instant::now();

    let result = timeout(WAIT, Tray::spawn(idle(), None, |_| {}))
        .await
        .expect("spawn hung");

    let err = result
        .err()
        .expect("spawn must fail when nothing owns the StatusNotifierWatcher name");
    println!("no-watcher error after {:?}: {err}", started.elapsed());
    assert!(err.to_string().contains("StatusNotifierWatcher"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "failed slowly: {:?}",
        started.elapsed()
    );
    std::io::stdout().flush().unwrap();
}

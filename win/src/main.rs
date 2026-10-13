//! CloudDrop's background agent for Windows (no console window). It runs the
//! upload engine, keeps the File Explorer cloud folder connected and shows the
//! notification-area icon. On Linux the same engine runs as `clouddrop run`.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    clouddrop_agent::agent::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("clouddrop-agent runs on Windows; on Linux the service is `clouddrop run`.");
    std::process::exit(1);
}

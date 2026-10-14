//! CloudDrop's Windows agent: the File Explorer cloud folder, the notification-area
//! icon, and startup. The executable is `src/main.rs`.

#[cfg(windows)]
pub mod agent;
#[cfg(windows)]
pub mod cloud;
#[cfg(windows)]
mod tray;

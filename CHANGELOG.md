# Changelog

## 0.2.1 — 2026-10-16

Linux and Windows on ARM64, and CloudDrop as a snap.

- Linux and Windows on ARM64: CI builds and tests CloudDrop natively on arm64 as well as
  x64 (Linux tests, Debian package and snap; Windows engine, Cloud Files test, app and
  packages). Releases carry `CloudDrop-<version>-windows-arm64.zip` next to the x64 zip.
  `snap/snapcraft.yaml` and `debian/` now build for arm64, and the Microsoft Store
  package declares the processor architecture it was built for.
- Snap (strict, amd64 and arm64): the upload engine and the top-bar icon start with the
  desktop session as autostart apps, since a snap has no systemd user service, and the
  setup assistant's "start at sign-in" switches them on and off.
- Only one upload engine runs at a time: a second `clouddrop run` exits with "CloudDrop
  is already running" instead of uploading in parallel, and only one top-bar icon is
  shown when more than one CloudDrop process is running.
- The desktop app needs libadwaita 1.5 instead of 1.6 (GNOME 46), so it also runs on
  older desktops.

## 0.2.0 — 2026-10-15

CloudDrop for Windows.

- Windows 10 (version 1809 or later) and Windows 11. The CloudDrop folder is a File
  Explorer cloud folder, in the navigation pane with Explorer's own status icons:
  uploaded files get the green check; **Free up space** keeps a file listed with a
  cloud icon and downloads it again when you open it; **Always keep on this device**
  downloads cloud-only files back and protects them from automatic space saving.
  The same Drive checksum check applies before any local data is removed.
- Windows app in Fluent design: setup assistant, Home, Files, Activity and Settings,
  plus a notification-area icon with pause/resume. CloudDrop starts at sign-in.
- Free in the Microsoft Store, signed and with the CloudDrop Google client built in.
  The zip on GitHub is unsigned and needs your own Google client, like source builds.
- Command line: `clouddrop logout`; `status --json` also works before setup and
  reports `configured`, `config_error`, `paused` and `root_id`;
  `service status --json`; `reset-cloud` restarts the service itself.

## 0.1.0 — 2026-10-14

First release.

- Upload-only sync of `~/CloudDrop` to a `CloudDrop` folder in Google Drive, keeping
  the folder structure; local renames and moves are mirrored without re-uploading,
  edits become new versions, and local deletes never delete in Drive.
- Resumable uploads that survive network drops, restarts and crashes; no duplicate
  files or folders after a crash or a lost local database.
- Free Up Space by hand or automatically (right after upload or after 7/30/90 days),
  with "always keep on this computer" folders; local copies are removed only after
  Google Drive's checksum matches. Download cloud-only files back at any time.
- Desktop app: setup assistant, Home (status, Google storage, space freed), Files
  browser with search, filters, multi-select and drag & drop, Activity history and
  Settings.
- Top-bar status icon with pause/resume.
- GNOME Files integration: status emblems, a Google Drive column, and right-click
  actions.
- `clouddrop` command-line tool for everything above.
- Uses only the `drive.file` permission: CloudDrop sees just the files it creates.

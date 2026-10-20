# Changelog

## Unreleased

A new look.

- New logo: a cloud on a sky-blue tile with a drop cut through it, on Linux, Windows, the
  Microsoft Store and the notification-area icons.
- Home: a status card in the logo's colours that shows overall progress, upload speed and
  time left while uploading, and the latest activity with *See All*.
- The status is on every screen: an icon in the header bar on Linux, a line under the logo
  in the navigation pane on Windows.
- Activity can show just uploads, freed space, downloads or problems.
- An empty folder in Files is a drop zone; *Free Up Space* has its own broom icon.
- About CloudDrop (main menu on Linux, Settings on Windows): version and links.
- Setup assistant: refreshed welcome page; on Windows, the steps are named.

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

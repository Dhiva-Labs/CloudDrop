# Changelog

## Unreleased

A new look, ARM64, and more clouds.

- **More clouds (preview)**: besides Google Drive, CloudDrop can upload to OneDrive, Dropbox,
  a WebDAV server, Nextcloud, S3-compatible storage (AWS, Backblaze B2, Wasabi, R2, MinIO…),
  an SFTP server and MEGA. Choose one in the setup assistant. The same promises hold for every
  one: upload-only, nothing deleted in the cloud, and space freed only after the cloud copy is
  proved identical, by the provider's own hash or by downloading it. They are tested against
  fake servers (SFTP against a real OpenSSH) but not yet with real accounts, and OneDrive and
  Dropbox need app registrations before release builds can sign in
  ([docs/PROVIDERS.md](docs/PROVIDERS.md)).
- **Proton Drive (experimental)**, through Proton's own command-line app (`proton-drive`),
  which signs in and does the encryption. Only builds from source offer it (`./install.sh`
  turns it on with `CLOUDDROP_EXPERIMENTAL_PROVIDERS=1`); the PPA, snap and Store builds
  don't, until Proton opens its SDK to third-party apps. This is a third-party application not
  officially supported by Proton. Tested against a fake of Proton's CLI, not a real account
  yet ([docs/PROVIDERS.md](docs/PROVIDERS.md#proton-drive)).
- Command line: `clouddrop providers`, `setup --provider <id> --set … --secret …`, `check`;
  `login` signs in to whichever provider is set up.

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
- Linux and Windows on ARM64: CI builds and tests CloudDrop natively on arm64 as well as
  x64 (Linux tests, Debian package and snap; Windows engine, Cloud Files test, app and
  packages). Releases carry `CloudDrop-<version>-windows-arm64.zip` next to the x64 zip.
  `snap/snapcraft.yaml` and `debian/` now build for arm64.

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

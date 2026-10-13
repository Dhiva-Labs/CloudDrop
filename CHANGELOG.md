# Changelog

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

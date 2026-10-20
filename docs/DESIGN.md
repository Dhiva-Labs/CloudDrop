# CloudDrop design

CloudDrop keeps a local folder (default `~/CloudDrop`) uploaded to Google Drive.
It is **upload-only**: anything added or edited locally is uploaded, with the
same folder structure, under a `CloudDrop` folder in My Drive. Deleting a file
locally never deletes it in Drive. The local copy is then "cloud-only" and can be
downloaded back at any time.

## Components

| Part | Language | Location |
| --- | --- | --- |
| `clouddrop` daemon + CLI | Rust | `src/` |
| Nautilus extension | Python (nautilus-python, `Nautilus` API 4.1) | `nautilus/clouddrop_nautilus.py` |
| Desktop app (setup assistant; Home, Files, Activity and Settings tabs) | Rust, GTK 4 + libadwaita | `gui/` → `clouddrop-gui` |
| Top-bar icon (StatusNotifierItem) | Rust, ksni | `tray/` (library used by the desktop app) |
| systemd user unit | | `packaging/clouddrop.service` |
| Installer | shell | `install.sh` |

The engine reaches the cloud only through `src/remote.rs`: `Remote` (one variant per provider)
and provider-neutral metadata, errors and content hashes. Google Drive's client is
`src/drive.rs`; [PROVIDERS.md](PROVIDERS.md) is the plan for the others.

## Files on disk

| What | Default path | Override |
| --- | --- | --- |
| Config | `$XDG_CONFIG_HOME/clouddrop/config.toml` (`~/.config/clouddrop/config.toml`) | `CLOUDDROP_CONFIG` = path to file |
| State database | `$XDG_DATA_HOME/clouddrop/state.db` (`~/.local/share/clouddrop/state.db`) | `CLOUDDROP_DATA_DIR` = directory |
| OAuth token | `$XDG_DATA_HOME/clouddrop/token.json` (mode 0600) | `CLOUDDROP_DATA_DIR` |
| Daemon status | `$XDG_DATA_HOME/clouddrop/status.json` | `CLOUDDROP_DATA_DIR` |

### config.toml

```toml
provider = "gdrive"                  # the cloud; absent = Google Drive, the only one so far
sync_dir = "/home/me/CloudDrop"     # absolute path
drive_folder = "CloudDrop"           # name of the top folder created in My Drive (`remote_folder` is read too)
client_id = "....apps.googleusercontent.com"
client_secret = "...."
parallel_uploads = 2
settle_secs = 2                      # a file must be quiet this long before upload
notifications = true
ignore = [".clouddrop-*", ".goutputstream-*", "*.part", "*.crdownload", "*.tmp", "~$*", ".~lock.*#", "*.swp", ".Trash-*"]
```

## State database (SQLite, WAL mode)

The database is the contract between the daemon, the CLI and the Nautilus
extension. Readers must open it read-only (`file:...?mode=ro` URI) and tolerate
`SQLITE_BUSY` by retrying or skipping.

```sql
CREATE TABLE entries (
  path        TEXT PRIMARY KEY,  -- relative to sync_dir, '/' separated, no leading or trailing slash
  kind        TEXT NOT NULL,     -- 'file' | 'dir'
  state       TEXT NOT NULL,     -- see below
  drive_id    TEXT,              -- Drive file/folder id once it exists in Drive
  cloud_name  TEXT,              -- name in Drive as last set
  cloud_parent TEXT,             -- Drive id of the parent folder as last set
  md5         TEXT,              -- md5 (hex) of the content last uploaded (= Drive md5Checksum)
  cloud_size  INTEGER,           -- size in bytes of the content last uploaded
  local_size  INTEGER,           -- local file metadata at last observation
  local_mtime_ns INTEGER,
  inode       INTEGER,
  upload_url  TEXT,              -- resumable upload session in progress
  upload_md5  TEXT,              -- md5 of the content that upload_url is uploading
  pin_prev    INTEGER NOT NULL DEFAULT 0, -- keep the previous Drive revision forever before replacing it
  move_pending INTEGER NOT NULL DEFAULT 0, -- cloud_only entry whose Drive name/parent must be updated
  attempts    INTEGER NOT NULL DEFAULT 0, -- consecutive failed attempts (drives the backoff)
  retry_at    INTEGER,           -- unix ms; do not retry before this
  error       TEXT,              -- last error message (state = 'error', or transient failure text)
  updated_at  INTEGER NOT NULL,  -- unix ms
  uploaded_at INTEGER,           -- unix ms the current Drive content was uploaded; NULL after a
                                 -- restore (automatic space saving skips it until re-uploaded)
  hash_kind   TEXT               -- which hash `md5` holds; NULL = MD5 (the only kind so far)
);
CREATE TABLE activity (          -- history for the desktop app; newest 10 000 rows kept
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,           -- unix ms
  kind TEXT NOT NULL,            -- uploaded | updated | moved | kept_in_cloud | freed |
                                 -- auto_freed | restored | failed
  path TEXT NOT NULL,
  bytes INTEGER,
  detail TEXT                    -- old path for moved, error for failed
);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- meta keys: 'schema_version' = '3' (version 1 databases gain uploaded_at and version 2 ones
--            hash_kind on open), 'sync_root' = absolute sync_dir,
--            'provider' = the cloud the ids belong to ('gdrive'); the engine refuses a database
--            bound to another provider, as it does one bound to another folder,
--            'root_id' = Drive id of the top CloudDrop folder,
--            'paused' = '1' while uploads are paused by the user
```

### Entry states

| state | Local file | Drive | Meaning |
| --- | --- | --- | --- |
| `pending` | present | missing or out of date | queued for upload (or rename/move in Drive) |
| `uploading` | present | in progress | upload running now |
| `synced` | present | same content | done |
| `cloud_only` | **absent** | present | removed locally, kept in Drive; can be restored |
| `error` | present | out of date | permanent failure, `error` has the message; `clouddrop retry` re-queues |

Directories use the same states (`synced` = folder exists in Drive with the
right name and parent). A directory whose descendants are all `cloud_only`
still exists locally, so the folder structure stays visible.

## Behaviour rules

- New or changed local file → `pending` → uploaded (new file, or new revision of the
  existing Drive file). Identical content is never re-uploaded (md5 compared).
- Local rename/move inside the sync folder → the Drive file/folder is renamed/moved
  (no re-upload). Moves without a paired rename event (moved into a just-renamed
  folder, or while the daemon was stopped) are matched by inode, plus size+mtime for files.
- A tracked file renamed over another tracked file: the overwritten one stays in Drive,
  renamed "name (2).ext", as a `cloud_only` entry. A never-uploaded file renamed over a
  tracked one (an editor's save-by-rename) is just an edit.
- Local delete → `cloud_only` if Drive has a copy, otherwise the entry is dropped.
  Drive is never modified by a delete.
- A new local file appearing at the path of a `cloud_only` entry: same md5 → `synced`;
  different → uploaded as a new revision, and the previous Drive revision is first
  pinned (`keepForever`) so the older content is never purged.
- `free` deletes local copies only after re-hashing the local file **and** confirming
  Drive's `md5Checksum`/size for that file id match and it is not trashed.
- `restore` downloads into a hidden temp file in the target folder, verifies md5,
  records the entry as `synced`, then renames into place.

## Automatic space saving

`auto_free_days` in the config (absent = never; 0 = right after upload; N = N days)
makes the daemon free local copies of `synced` files whose upload *and* last local
change are at least that old, checking every 30 s and right after uploads in mode 0.
Folders in `keep_local` (and everything beneath them) are never freed automatically.
Each candidate goes through a free job: re-hash the local file, compare with the
entry's md5, then `files.get` the Drive copy (md5, size, not trashed). Only then does
the engine delete the file — after re-checking inode, size and mtime are still the
ones that were hashed. If Drive's copy is gone, the file is uploaded again as a new
Drive file; if it was changed elsewhere, the local content is uploaded as a new
version of the same file with the other version pinned (`keepForever`). Manual
`free` follows the same rules.

## Daemon status and pause

The daemon rewrites `status.json` when its state changes (at most every 0.5 s) and
at least every 2 s as a heartbeat:

```json
{"pid": 1234, "updated_ms": 1791913161130, "phase": "uploading",
 "message": null, "uploads": [{"path": "Photos/beach.jpg", "sent": 524288, "total": 3000000}],
 "waiting": 4}
```

It also carries `quota` (`{"limit": …, "usage": …, "usage_in_drive": …}` bytes,
refreshed every 10 minutes from `about.get`; `limit` null = unlimited).

`phase` is one of `starting`, `idle`, `waiting`, `uploading`, `paused`, `offline`,
`needs_sign_in`, `drive_folder_missing`. Readers treat the file as stale when
`updated_ms` is older than 10 s or the pid is gone (daemon not running); it is
removed on clean exit.

Pausing sets meta `paused` = `1` (`clouddrop pause`, the app, or the top-bar
icon). Within a tick the daemon stops scheduling and cancels running uploads; their
resumable sessions are kept, so `resume` continues where each upload stopped.

## CLI (used by the Nautilus extension)

All path arguments may be absolute or relative to the current directory and must
be inside `sync_dir`. Commands exit 0 on success and print a short human message;
failures exit non-zero with the message on stderr.

| Command | Effect |
| --- | --- |
| `clouddrop setup --client-id ID --client-secret S [--folder DIR]` | write config, sign in |
| `clouddrop login` | (re)run Google sign-in |
| `clouddrop run` | the daemon (systemd user service) |
| `clouddrop status [--json]` | counts per state, errors |
| `clouddrop free PATH...` | free up space: delete verified local copies (dirs: all synced files inside, folders are kept) |
| `clouddrop restore PATH...` | download cloud-only files back (a dir restores all cloud-only files beneath it) |
| `clouddrop open PATH` | open the file/folder in Drive in the browser |
| `clouddrop url PATH` | print the Drive URL |
| `clouddrop retry [PATH...]` | re-queue `error` entries (all, or under PATH) |
| `clouddrop pause` / `resume` | pause or resume uploading |
| `clouddrop auto-free never\|now\|DAYS` | set automatic space saving (restarts the service) |
| `clouddrop keep PATH...` / `unkeep PATH...` | add/remove "always keep on this computer" folders |
| `clouddrop ls-cloud DIR [--json]` | list cloud-only entries directly inside DIR |
| `clouddrop reset-cloud --yes` | after the Drive folder was permanently deleted: forget Drive ids and upload everything again |

Drive URLs: files `https://drive.google.com/file/d/<id>/view`, folders
`https://drive.google.com/drive/folders/<id>`.

## Google API

- OAuth 2.0 installed-app flow with loopback redirect + PKCE; scope
  `https://www.googleapis.com/auth/drive.file` (only files the app created; no
  restricted-scope audit needed).
- Drive v3 REST: folders via `files.create`, content via resumable upload sessions
  (8 MiB chunks, sessions persisted in `upload_url` so restarts resume), renames/moves
  via `files.update` with `addParents`/`removeParents`, downloads via `alt=media`.
- 401 → refresh token once; 429/5xx/rate-limit 403 → exponential backoff via `retry_at`
  (5 s doubling to 15 min); other 4xx → `error`. A rejected refresh token pauses all
  uploads until `clouddrop login` rewrites the token file.
- Creating a folder or file first looks for an identical one (same name in the same
  parent; files also same md5) and adopts it, so a crash between "created in Drive" and
  "recorded locally", or a lost database, never produces duplicates.

## Concurrency

The daemon's engine task is the only writer of entry rows while it runs. Drive jobs
run concurrently but only report results back through a channel; an entry renamed or
deleted while its job runs is re-keyed in the engine's in-flight table, so results land
on the entry's current path. The CLI's `free` and `restore` edit only idle entries
(`synced` / `cloud_only`), and the engine re-reads rows rather than caching them.

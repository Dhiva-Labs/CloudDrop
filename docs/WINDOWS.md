# CloudDrop on Windows

Same product, same Rust engine, Windows-native shell.

## Pieces

| Piece | What | Where |
| --- | --- | --- |
| `clouddrop.exe` | the CLI (console subsystem) — same commands as on Linux | root crate |
| `clouddrop-agent.exe` | the background engine (GUI subsystem: no console) started at sign-in; hosts the File Explorer cloud-folder integration (Cloud Files API) and the notification-area icon | `win/` crate |
| CloudDrop app | the window: setup assistant, Home, Files, Activity, Settings — Flutter with Fluent design | `winapp/` |

The agent and the app never link to each other. Like the Linux Files extension, the
app talks to the engine through files and the CLI (see the contract below), so the app
can be developed and tested on Linux against the Linux daemon.

## Explorer integration (Cloud Files API)

`%USERPROFILE%\CloudDrop` is registered as a cloud sync root, so it appears in
Explorer's navigation pane with CloudDrop's icon, and every item shows the standard
cloud states:

| CloudDrop state | Explorer |
| --- | --- |
| `pending` / `uploading` | sync arrows |
| `synced` | green check (available on this device) |
| `cloud_only` (freed) | cloud icon — a **placeholder**: still listed, downloads when opened |
| kept folder (`keep_local`) | "Always keep on this device" (pinned) |

Explorer's own **Free up space** dehydrates a file (→ `cloud_only`, after the same
Drive checksum check as everywhere else) and **Always keep on this device** pins it
(→ `keep_local`). Upload-only rules are unchanged: deleting in Explorer never deletes
in Drive; deleted items disappear from Explorer and stay listed (cloud-only) in the
app's Files tab, where they can be downloaded back.

## Paths

| What | Windows | Linux | Override |
| --- | --- | --- | --- |
| config | `%APPDATA%\clouddrop\config.toml` | `~/.config/clouddrop/config.toml` | `CLOUDDROP_CONFIG` |
| state db, status, token | `%LOCALAPPDATA%\clouddrop\` | `~/.local/share/clouddrop/` | `CLOUDDROP_DATA_DIR` |
| synced folder | `%USERPROFILE%\CloudDrop` | `~/CloudDrop` | config `sync_dir` |

## App ↔ engine contract

Read (never write) the state database (`state.db`, SQLite, WAL; open read-only) and
`status.json` as described in [DESIGN.md](DESIGN.md). Do everything else through the
CLI (`clouddrop.exe` next to the app's exe, else on `PATH`, else `CLOUDDROP_BIN`):

| Need | Command |
| --- | --- |
| overall state (signed in, built-in client, counts, daemon status, quota) | `clouddrop status --json` |
| settings (secret withheld) | `clouddrop config get --json` |
| change a setting (applied live) | `clouddrop config set KEY VALUE` — `parallel_uploads`, `settle_secs`, `notifications`, `ignore`, `auto_free_days` (`never`/`now`/N), `keep_local` (comma list) |
| first-run setup | `clouddrop setup --no-login [--folder DIR] [--client-json FILE]` then `clouddrop login --no-browser` (prints the consent URL; open it; exits 0 when signed in) |
| sign in again | `clouddrop login` |
| free / download back / retry | `clouddrop free PATH…`, `clouddrop restore PATH…`, `clouddrop retry [PATH…]` |
| pause / resume | `clouddrop pause`, `clouddrop resume` |
| keep on this computer | `clouddrop keep PATH…`, `clouddrop unkeep PATH…` |
| background service | `clouddrop service status|start|stop|enable|disable` |
| Drive link | `clouddrop url PATH` |

Commands print a human sentence on success (exit 0) or an error on stderr (exit 1).

## Distribution

- **Microsoft Store** (MSIX, signed by the Store): built with the CloudDrop Google
  client compiled in; the agent starts at sign-in through the package's startup task.
- **GitHub**: source only, no client — source builds create their own (see README).

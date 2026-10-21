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
cloud states. Every other connection's folder is a sync root of its own
(`CloudDrop!<SID>!<name>`), shown as "CloudDrop - <name>"; removing the connection
unregisters it, as uninstalling does ([PROVIDERS.md](PROVIDERS.md#several-clouds-at-once)):

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
| another connection NAME | `connections\NAME\` beside config.toml and in the data folder | the same, `/` | |

## App ↔ engine contract

Binary lookup order: `CLOUDDROP_BIN`, then `clouddrop.exe` next to the app's exe, then
`PATH`.

Read (never write) the state database (`state.db`, SQLite, WAL; open read-only) and
`status.json` as described in [DESIGN.md](DESIGN.md). Do everything else through the
CLI:

| Need | Command |
| --- | --- |
| overall state: `configured` (with `config_error` when the settings file can't be read), `signed_in`, `has_builtin_client`, `paused`, `root_id`, counts, `daemon` (status incl. quota) — works before setup too | `clouddrop status --json` |
| settings (secret withheld) | `clouddrop config get --json` |
| change a setting (applied live) | `clouddrop config set KEY VALUE` — `parallel_uploads`, `settle_secs`, `notifications`, `ignore`, `auto_free_days` (`never`/`now`/N), `keep_local` (comma list) |
| first-run setup | `clouddrop setup --no-login [--folder DIR] [--client-json FILE]` then `clouddrop login --no-browser` (prints the consent URL; open it; exits 0 when signed in) |
| sign in again / sign out | `clouddrop login`, `clouddrop logout` (stops the service, forgets the sign-in) |
| free / download back / retry | `clouddrop free PATH…`, `clouddrop restore PATH…`, `clouddrop retry [PATH…]` |
| pause / resume | `clouddrop pause`, `clouddrop resume` |
| keep on this computer | `clouddrop keep PATH…`, `clouddrop unkeep PATH…` |
| background service | `clouddrop service start\|stop\|enable\|disable`; `clouddrop service status --json` → `{"running", "at_login"}` |
| Drive link | `clouddrop url PATH` |

Commands print a human sentence on success (exit 0) or an error on stderr (exit 1).

With several connections, `clouddrop connections --json` lists them (each with its
`data_dir`, where its `state.db` and `status.json` are), and every command above takes
`--connection NAME`; the app always passes it. `connections add NAME --folder …` and
`connections remove NAME` add and remove one; `logout` stops only that connection's
engine, and `service …` is the one background agent that runs them all.

## Distribution

- **Microsoft Store** (free; MSIX signed by the Store): built with the
  CloudDrop Google client compiled in (`packaging/windows/build.ps1` with
  `packaging/oauth-client.env`, then `packaging/windows/msix/build-msix.ps1`). The agent
  starts at sign-in through the package's startup task (`CloudDropAgent`), which
  `clouddrop service enable|disable` switches; `clouddrop` works in terminals through
  an app execution alias. CI installs a test-signed copy and runs the cloud-folder
  self-test (`clouddrop-agent --selftest DIR`) inside the package.
- **GitHub** (free): source, plus the CI-built zip (`install.cmd`; unsigned, no Google
  client — like source builds, the setup assistant asks for your own).
- **x64 and arm64**: every package exists for both Intel/AMD PCs and Windows on ARM PCs
  (`CloudDrop-<version>-windows-x64.zip` and `…-windows-arm64.zip`). Nothing is
  cross-compiled or emulated: CI builds each one on a native runner, runs the Rust
  tests, the Cloud Files test and the app on it, and checks the machine type of every
  `.exe` and `.dll` (and of the app's compiled Dart code). `packaging/windows/build.ps1`
  and `msix/build-msix.ps1` build for the PC they run on; an arm64 Store package is built
  on a Windows on ARM PC (or the arm64 CI runner) the same way as the x64 one.

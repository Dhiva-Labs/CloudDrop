# CloudDrop for Windows (the app)

The window of CloudDrop on Windows 11: a setup assistant, **Home**, **Files**, **Activity**
and **Settings**, in Fluent design (`fluent_ui`: NavigationView, CommandBar, InfoBar,
ContentDialog, Mica). It also runs on Linux, which is how it is developed and tested.

The app never links to the engine. It follows the contract in
[docs/WINDOWS.md](../docs/WINDOWS.md): it reads `state.db` (read-only) and `status.json`,
and does everything else by running the `clouddrop` CLI.

```
lib/data/      engine contract: state.db reader, status.json, CLI runner, EngineApi, file copying
lib/logic/     pure rules: sizes and dates, Home summary, Files sorting and capabilities, wording
lib/state/     ChangeNotifier controllers (app, files, activity, setup assistant)
lib/ui/        the screens (pages/, setup/, widgets/) and the Fluent theme
test/          unit, widget and golden tests; test/integration runs the real CLI
```

What the app runs (all through `lib/data/engine_api.dart`):

| Need | Command |
| --- | --- |
| overall state, also before setup | `status --json`: `configured`, `signed_in`, `has_builtin_client`, `paused`, `root_id`, counts, `daemon` |
| settings | `config get --json`, `config set KEY VALUE` |
| first run | `setup --no-login --folder=… [--client-json=…]`, then `login --no-browser` (the app opens the link) |
| sign out | `logout` (stops the service, forgets the sign-in), then the assistant restarts at its sign-in page |
| background service | `service status --json` → `{running, at_login}`, `service start\|enable\|disable` |
| files | `free`, `restore`, `retry`, `pause`, `resume`, `keep`, `unkeep`, `url` |
| the folders (connections) | `connections --json` → `[{name, label, folder, provider, data_dir, configured, error, …}]` |
| another folder | `connections add NAME --folder=… --drive-folder=… [--provider=…] [--set K=V] [--secret K=V]`, then `login --no-browser` or `check` with `--connection NAME` |
| remove a folder | `connections remove NAME` (the folder and the cloud keep everything) |

Several folders can upload side by side, each a **connection** with its own settings, sign-in,
`state.db` and `status.json` (in its `data_dir`; see
[PROVIDERS.md](../docs/PROVIDERS.md#several-clouds-at-once)). Every command about one folder
carries `--connection NAME` (also `default`), so the app's working directory never matters;
`connections`, `providers` and `service` are the whole installation's. With more than one
folder the navigation pane lists them above Home, each with its status dot (a drop-down above
the screen in a narrow window), and "Add Another Folder…" runs the setup assistant for a new
connection named after its folder; Settings has "Add Another Folder…" and "Remove This
Folder…". The folder shown is remembered in `app.json` in the data directory
(`{"connection": name}`, shared with the GTK app); if it is gone, the first one is shown. An
engine from before connections (no `connections` command) is used as before, without
`--connection`.

On Windows a freed item stays in the sync folder as a Cloud Files placeholder that downloads when
opened, so the Files screen simply opens it; only an item that is gone from the folder offers
*Download* (`restore`). On Linux a freed file is gone, so it always offers *Download*.

Files, in the order the app looks for them (override with the usual variables):

| What | Windows | Linux | Override |
| --- | --- | --- | --- |
| config | `%APPDATA%\clouddrop\config.toml` | `~/.config/clouddrop/config.toml` | `CLOUDDROP_CONFIG` |
| state db, status | `%LOCALAPPDATA%\clouddrop\` | `~/.local/share/clouddrop/` | `CLOUDDROP_DATA_DIR` |
| another connection NAME | `connections\NAME\` beside config.toml and in the data folder | the same | |
| CLI | `CLOUDDROP_BIN`, else `clouddrop.exe` next to the app, else `PATH` | same, `clouddrop` | `CLOUDDROP_BIN` |

## Build

```
flutter build windows       # on Windows; then put clouddrop.exe next to clouddrop_app.exe
flutter build linux         # development build
```

On a Windows on ARM PC `flutter build windows` produces an arm64 app (in
`build\windows\arm64\runner\Release`); on an Intel/AMD PC it produces x64. The engine
(`cargo build --release -p clouddrop -p clouddrop-agent`) is native in the same way, and
`packaging\windows\build.ps1` assembles the matching package.

The app's own exe is `clouddrop_app.exe` on purpose: Windows file names are
case-insensitive, so `CloudDrop.exe` would collide with the engine's `clouddrop.exe`.

## Try it without touching your real CloudDrop

Point everything at a sandbox first, never at your real config and data:

```
cargo build -p clouddrop --example mock_drive && cargo build -p clouddrop
target/debug/examples/mock_drive 127.0.0.1:8299 &           # fake Google Drive
export CLOUDDROP_BIN=$PWD/target/debug/clouddrop
export CLOUDDROP_CONFIG=/tmp/cd-sandbox/config.toml CLOUDDROP_DATA_DIR=/tmp/cd-sandbox/data
cd winapp && flutter run -d linux
```

## Tests

```
flutter analyze
flutter test                                 # everything (the integration test skips itself if the CLI isn't built)
flutter test --tags integration test/integration
flutter test --update-goldens test/ui        # regenerate the screenshots in test/goldens/
```

`store/screenshots/` holds the Microsoft Store listing screenshots (1920x1080, light, 150% scaling, sample
data from `test/store/store_data.dart`). They are not part of the normal run (the tests skip
themselves); regenerate them with `flutter test --tags store --update-goldens test/store`.

`test/ui/*golden*` render every screen, light and dark, 1100x760 and 480 wide, with a bundled
open font (`test/fonts`: Noto Sans, Liberation Sans for symbols; test only).
`test/integration` starts the real `clouddrop run` and the fake Drive (on a free port) in a sandbox
with an empty environment and drives the app's data layer against them, with one folder and with
two (connections).

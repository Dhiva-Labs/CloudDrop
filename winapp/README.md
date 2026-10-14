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

On Windows a freed item stays in the sync folder as a Cloud Files placeholder that downloads when
opened, so the Files screen simply opens it; only an item that is gone from the folder offers
*Download* (`restore`). On Linux a freed file is gone, so it always offers *Download*.

Files, in the order the app looks for them (override with the usual variables):

| What | Windows | Linux | Override |
| --- | --- | --- | --- |
| config | `%APPDATA%\clouddrop\config.toml` | `~/.config/clouddrop/config.toml` | `CLOUDDROP_CONFIG` |
| state db, status | `%LOCALAPPDATA%\clouddrop\` | `~/.local/share/clouddrop/` | `CLOUDDROP_DATA_DIR` |
| CLI | `CLOUDDROP_BIN`, else `clouddrop.exe` next to the app, else `PATH` | same, `clouddrop` | `CLOUDDROP_BIN` |

## Build

```
flutter build windows       # on Windows; then put clouddrop.exe next to clouddrop_app.exe
flutter build linux         # development build
```

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
`test/integration` starts the real `clouddrop run` and the fake Drive in a sandbox with an empty
environment and drives the app's data layer against them.

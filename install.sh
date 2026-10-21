#!/usr/bin/env bash
# Install (or uninstall) CloudDrop for the current user.
#
#   ./install.sh                                  build with cargo and install
#   ./install.sh --no-build --binary PATH         install a prebuilt binary
#   ./install.sh --no-build --binary PATH --gui-binary PATH
#                                                 ... and a prebuilt desktop app
#   ./install.sh --uninstall                      remove everything but config/data
#
# Set SKIP_SYSTEMCTL=1 to skip every systemctl call (used for testing).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Builds from source offer the experimental providers (Proton Drive, through Proton's CLI);
# packaged builds (PPA, snap, Store) never do. CLOUDDROP_EXPERIMENTAL_PROVIDERS=0 leaves them out.
export CLOUDDROP_EXPERIMENTAL_PROVIDERS="${CLOUDDROP_EXPERIMENTAL_PROVIDERS:-1}"

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"

BIN_DIR="$HOME/.local/bin"
BIN_PATH="$BIN_DIR/clouddrop"
GUI_PATH="$BIN_DIR/clouddrop-gui"
EXT_DIR="$DATA_HOME/nautilus-python/extensions"
EXT_PATH="$EXT_DIR/clouddrop_nautilus.py"
UNIT_DIR="$CONFIG_HOME/systemd/user"
UNIT_PATH="$UNIT_DIR/clouddrop.service"
APP_ID="com.dhivalabs.CloudDrop"
ICON_DIR="$DATA_HOME/icons/hicolor"
APPS_DIR="$DATA_HOME/applications"
DESKTOP_PATH="$APPS_DIR/$APP_ID.desktop"
METAINFO_PATH="$DATA_HOME/metainfo/$APP_ID.metainfo.xml"
AUTOSTART_PATH="$CONFIG_HOME/autostart/$APP_ID.desktop"
CONFIG_FILE="${CLOUDDROP_CONFIG:-$CONFIG_HOME/clouddrop/config.toml}"
STATE_DIR="${CLOUDDROP_DATA_DIR:-$DATA_HOME/clouddrop}"

usage() {
    cat <<'EOF'
Usage: install.sh [--no-build] [--binary PATH] [--gui-binary PATH] [--uninstall]

  --no-build          do not run cargo; use --binary PATH / --gui-binary PATH, or
                      target/release/clouddrop and target/release/clouddrop-gui
  --binary PATH       install this prebuilt clouddrop binary (implies --no-build;
                      the desktop app is then only installed with --gui-binary)
  --gui-binary PATH   install this prebuilt clouddrop-gui desktop app
  --uninstall         stop the service and remove the binaries, unit, extension,
                      icons and launcher; configuration and synced-state data
                      are kept
  -h, --help          show this help

The desktop app (clouddrop-gui) is built when GTK 4 and libadwaita development
files are installed:  sudo apt install libgtk-4-dev libadwaita-1-dev

Environment: SKIP_SYSTEMCTL=1 skips all systemctl calls.
EOF
}

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

systemctl_user() {
    if [[ "${SKIP_SYSTEMCTL:-}" == 1 ]]; then
        say "skipping: systemctl --user $*"
        return 0
    fi
    systemctl --user "$@"
}

service_active() {
    [[ "${SKIP_SYSTEMCTL:-}" != 1 ]] && command -v systemctl >/dev/null 2>&1 &&
        systemctl --user is-active --quiet clouddrop
}

check_nautilus_python() {
    if command -v dpkg >/dev/null 2>&1; then
        if ! dpkg -s python3-nautilus >/dev/null 2>&1; then
            warn "python3-nautilus is not installed; the Files integration needs it."
            warn "  sudo apt install python3-nautilus"
        fi
    else
        warn "could not check for nautilus-python; install it for your distribution."
    fi
}

check_path() {
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) warn "$BIN_DIR is not in PATH; add it to run 'clouddrop' from a shell." ;;
    esac
}

install_files() {
    local binary="$1"
    install -Dm755 "$binary" "$BIN_PATH"
    install -Dm644 "$SCRIPT_DIR/nautilus/clouddrop_nautilus.py" "$EXT_PATH"
    install -Dm644 "$SCRIPT_DIR/packaging/clouddrop.service" "$UNIT_PATH"
    say "installed $BIN_PATH"
    say "installed $EXT_PATH"
    say "installed $UNIT_PATH"
}

# Prints the path of the desktop app binary to install, or nothing to skip it.
# $1 = prebuilt path or empty, $2 = 1 to build, $3 = 1 if the default prebuilt
# path (target/release/clouddrop-gui) may be used.
resolve_gui_binary() {
    local given="$1" build="$2" use_default="$3"
    local built="$SCRIPT_DIR/target/release/clouddrop-gui"

    if [[ -n "$given" ]]; then
        [[ -f "$given" ]] || die "desktop app binary not found: $given"
        printf '%s\n' "$given"
    elif ((build)); then
        if [[ ! -f "$SCRIPT_DIR/gui/Cargo.toml" ]]; then
            warn "gui/Cargo.toml not found; skipping the desktop app."
        elif ! command -v pkg-config >/dev/null 2>&1 || ! pkg-config --exists gtk4 libadwaita-1; then
            warn "GTK 4 / libadwaita development files not found; skipping the desktop app."
            warn "  sudo apt install pkg-config libgtk-4-dev libadwaita-1-dev"
        elif ! (cd "$SCRIPT_DIR" && cargo build --release -p clouddrop-gui) >&2; then
            warn "building the desktop app failed; continuing without it."
        else
            printf '%s\n' "$built"
        fi
    elif ((use_default)) && [[ -f "$built" ]]; then
        printf '%s\n' "$built"
    else
        warn "no desktop app binary (use --gui-binary PATH); skipping the desktop app."
    fi
}

# Desktop Entry Exec values need quoting when they contain spaces.
desktop_exec() {
    case "$1" in
        *[[:space:]]*) printf '"%s"' "$1" ;;
        *) printf '%s' "$1" ;;
    esac
}

# Calls "$@" <path relative to data/icons/hicolor> for every icon file.
for_each_icon() {
    local src="$SCRIPT_DIR/data/icons/hicolor" file
    [[ -d "$src" ]] || return 0
    while IFS= read -r -d '' file; do
        "$@" "${file#"$src"/}"
    done < <(find "$src" -type f -print0)
}

install_icon() { install -Dm644 "$SCRIPT_DIR/data/icons/hicolor/$1" "$ICON_DIR/$1"; }
remove_icon() { rm -f "$ICON_DIR/$1"; }

install_desktop_files() {
    local line
    for_each_icon install_icon

    # The launcher must point at the installed binary, not rely on PATH.
    mkdir -p "$APPS_DIR"
    while IFS= read -r line || [[ -n "$line" ]]; do
        case "$line" in
            Exec=*) printf 'Exec=%s\n' "$(desktop_exec "$GUI_PATH")" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done <"$SCRIPT_DIR/data/$APP_ID.desktop" >"$DESKTOP_PATH"
    chmod 644 "$DESKTOP_PATH"
    install -Dm644 "$SCRIPT_DIR/data/$APP_ID.metainfo.xml" "$METAINFO_PATH"

    refresh_caches
    say "installed $GUI_PATH"
    say "installed $DESKTOP_PATH"
    say "installed icons in $ICON_DIR"
    say "installed $METAINFO_PATH"
}

# Both caches are optional speed-ups; a failure must not fail the install.
refresh_caches() {
    if [[ -d "$ICON_DIR" ]]; then
        if [[ -z "$(find "$ICON_DIR" -type f ! -name icon-theme.cache -print -quit)" ]]; then
            # Only our (now removed) icons lived here; drop the stale cache and dirs.
            rm -f "$ICON_DIR/icon-theme.cache"
            find "$ICON_DIR" -depth -type d -empty -delete 2>/dev/null || true
        elif command -v gtk-update-icon-cache >/dev/null 2>&1; then
            gtk-update-icon-cache -q -t -f "$ICON_DIR" || true
        fi
    fi
    if [[ -d "$APPS_DIR" ]] && command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$APPS_DIR" || true
    fi
}

do_install() {
    local build=1 binary="" gui_binary=""
    while (($#)); do
        case "$1" in
            --no-build) build=0 ;;
            --binary)
                (($# >= 2)) || die "--binary needs a path"
                binary="$2"
                build=0
                shift
                ;;
            --gui-binary)
                (($# >= 2)) || die "--gui-binary needs a path"
                gui_binary="$2"
                shift
                ;;
            *) usage >&2; die "unknown option: $1" ;;
        esac
        shift
    done

    # target/release/clouddrop-gui is only picked up when no explicit --binary was given.
    local use_default_gui=0
    if [[ -z "$binary" ]]; then
        use_default_gui=1
        binary="$SCRIPT_DIR/target/release/clouddrop"
        if ((build)); then
            command -v cargo >/dev/null 2>&1 ||
                die "cargo not found; install Rust or use --no-build --binary PATH"
            # Ready-to-use builds carry the CloudDrop OAuth client if it was set up
            # with packaging/set-oauth-client.sh (never committed to git).
            if [[ -f "$SCRIPT_DIR/packaging/oauth-client.env" ]]; then
                set -a
                # shellcheck disable=SC1091
                source "$SCRIPT_DIR/packaging/oauth-client.env"
                set +a
            fi
            (cd "$SCRIPT_DIR" && cargo build --release)
        fi
    fi
    [[ -f "$binary" ]] || die "binary not found: $binary"

    local gui_built
    gui_built="$(resolve_gui_binary "$gui_binary" "$build" "$use_default_gui")"

    local was_active=0
    service_active && was_active=1

    install_files "$binary"
    local have_gui=0
    if [[ -n "$gui_built" ]]; then
        install -Dm755 "$gui_built" "$GUI_PATH"
        install_desktop_files
        have_gui=1
    fi
    check_nautilus_python
    check_path

    systemctl_user daemon-reload
    if [[ -f "$CONFIG_FILE" ]]; then
        systemctl_user enable --now clouddrop
        # enable --now leaves an already-running daemon on the old binary.
        if ((was_active)); then
            systemctl_user restart clouddrop
        fi
    elif ((have_gui)); then
        # Setting up in the app enables the service, so it stays off until then.
        say
        say "Open CloudDrop from your app grid to finish setup."
        say "Or in a terminal: clouddrop setup --client-id ID --client-secret SECRET && systemctl --user enable --now clouddrop"
    else
        say
        say "CloudDrop is not configured yet. Next steps:"
        say "  clouddrop setup --client-id ID --client-secret SECRET"
        say "  systemctl --user enable --now clouddrop"
    fi

    if ((have_gui)); then
        say
        say "The top-bar icon needs a StatusNotifier host: Ubuntu has one built in;"
        say "on other GNOME systems install the AppIndicator shell extension."
    fi
    say
    say "Restart Files to load the extension: nautilus -q"
}

do_uninstall() {
    systemctl_user disable --now clouddrop || true
    rm -f "$BIN_PATH" "$GUI_PATH" "$EXT_PATH" "$UNIT_PATH" \
        "$DESKTOP_PATH" "$METAINFO_PATH" "$AUTOSTART_PATH"
    for_each_icon remove_icon
    refresh_caches
    systemctl_user daemon-reload || true
    say "removed $BIN_PATH"
    say "removed $GUI_PATH"
    say "removed $EXT_PATH"
    say "removed $UNIT_PATH"
    say "removed $DESKTOP_PATH"
    say "removed $METAINFO_PATH"
    say "removed icons from $ICON_DIR"
    say "removed $AUTOSTART_PATH"
    say
    say "Kept your configuration:  $CONFIG_FILE"
    say "Kept your state and token: $STATE_DIR"
    say "Restart Files to unload the extension: nautilus -q"
}

main() {
    case "${1:-}" in
        -h | --help) usage ;;
        --uninstall)
            (($# == 1)) || die "--uninstall takes no other options"
            do_uninstall
            ;;
        *) do_install "$@" ;;
    esac
}

main "$@"

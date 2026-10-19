#!/bin/sh
# Install or uninstall Filester Manager for the current user or system-wide.
#
#   ./install.sh                  per-user install into ~/.local (no root)
#   ./install.sh --system         system-wide install into /usr/local (uses sudo)
#   ./install.sh --prefix DIR     install into DIR/bin and DIR/share
#   ./install.sh --uninstall      remove what the matching install put in place
#   ./install.sh --uninstall --purge
#                                 also delete your Filester Manager settings and
#                                 download cache (per-user mode only)
#
# Run it from a source checkout (it builds with cargo) or from an unpacked
# release tarball (it uses the bundled bin/filester-app). The build always
# runs as the invoking user; only the final copy is elevated for --system.
# Installed files:
#   <bin>/filester-app
#   <share>/applications/io.github.scroguard.Filester.desktop
#   <share>/metainfo/io.github.scroguard.Filester.metainfo.xml
#   <share>/icons/hicolor/scalable/apps/io.github.scroguard.Filester.svg

set -eu

APP_ID=io.github.scroguard.Filester
BIN_NAME=filester-app
SRC_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

usage() {
    # Print the header comment block.
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
    exit "${1:-0}"
}

# --- Arguments ---------------------------------------------------------------

mode=user
action=install
purge=0
prefix=
while [ $# -gt 0 ]; do
    case $1 in
        --user) mode=user ;;
        --system) mode=system ;;
        --prefix)
            [ $# -ge 2 ] || die "--prefix needs a directory"
            mode=prefix
            prefix=$2
            shift
            ;;
        --prefix=*)
            mode=prefix
            prefix=${1#--prefix=}
            ;;
        --uninstall) action=uninstall ;;
        --purge) purge=1 ;;
        -h | --help) usage 0 ;;
        *)
            warn "unknown option: $1"
            usage 2
            ;;
    esac
    shift
done

case $mode in
    user)
        [ "$(id -u)" -ne 0 ] || die "per-user install as root would install for root. Run without sudo, or use --system."
        [ -n "${HOME:-}" ] || die "HOME is not set"
        bin_dir=$HOME/.local/bin
        share_dir=${XDG_DATA_HOME:-$HOME/.local/share}
        ;;
    system)
        bin_dir=/usr/local/bin
        share_dir=/usr/local/share
        ;;
    prefix)
        [ -n "$prefix" ] || die "--prefix needs a directory"
        case $prefix in /*) ;; *) prefix=$(pwd)/$prefix ;; esac
        bin_dir=$prefix/bin
        share_dir=$prefix/share
        ;;
esac
[ "$purge" -eq 0 ] || [ "$action" = uninstall ] || die "--purge only applies to --uninstall"

bin_path=$bin_dir/$BIN_NAME
desktop_path=$share_dir/applications/$APP_ID.desktop
metainfo_path=$share_dir/metainfo/$APP_ID.metainfo.xml
icon_path=$share_dir/icons/hicolor/scalable/apps/$APP_ID.svg

# Run a command with root rights when the target needs them.
as_target() {
    if [ "$mode" = system ] && [ "$(id -u)" -ne 0 ]; then
        command -v sudo >/dev/null 2>&1 || die "sudo is needed for --system (or run as root)"
        sudo -- "$@"
    else
        "$@"
    fi
}

# Distro-specific package hint, from /etc/os-release.
distro_family() {
    ids=
    if [ -r /etc/os-release ]; then
        # shellcheck disable=SC1091
        ids=$(. /etc/os-release && printf '%s %s' "${ID:-}" "${ID_LIKE:-}")
    fi
    case " $ids " in
        *" arch "*) echo arch ;;
        *" debian "* | *" ubuntu "*) echo debian ;;
        *" fedora "* | *" rhel "* | *" centos "*) echo fedora ;;
        *) echo unknown ;;
    esac
}

hint_runtime() {
    case $(distro_family) in
        arch) say "  sudo pacman -S --needed gtk4 libadwaita" ;;
        debian) say "  sudo apt install libgtk-4-1 libadwaita-1-0" ;;
        fedora) say "  sudo dnf install gtk4 libadwaita" ;;
        *) say "  Install GTK 4 (>= 4.14) and libadwaita (>= 1.5) with your package manager." ;;
    esac
}

hint_build() {
    case $(distro_family) in
        arch) say "  sudo pacman -S --needed base-devel pkgconf gtk4 libadwaita rustup git" ;;
        debian) say "  sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev rustup git" ;;
        fedora) say "  sudo dnf install gcc gcc-c++ make pkgconf-pkg-config gtk4-devel libadwaita-devel rustup git" ;;
        *) say "  Install a C toolchain, pkg-config, GTK 4 and libadwaita development files, and rustup." ;;
    esac
    say "  rustup default stable"
}

refresh_caches() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        as_target update-desktop-database -q "$share_dir/applications" || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -f "$share_dir/icons/hicolor/index.theme" ]; then
        as_target gtk-update-icon-cache -q -t -f "$share_dir/icons/hicolor" || true
    fi
}

# --- Uninstall ---------------------------------------------------------------

if [ "$action" = uninstall ]; then
    removed=0
    for f in "$bin_path" "$desktop_path" "$metainfo_path" "$icon_path"; do
        if [ -e "$f" ] || [ -L "$f" ]; then
            as_target rm -f -- "$f"
            say "removed $f"
            removed=$((removed + 1))
        fi
    done
    [ "$removed" -gt 0 ] && refresh_caches
    [ "$removed" -gt 0 ] || say "Nothing to remove under $bin_dir and $share_dir."

    config_dir=${XDG_CONFIG_HOME:-${HOME:-~}/.config}/$APP_ID
    cache_dir=${XDG_CACHE_HOME:-${HOME:-~}/.cache}/$APP_ID
    if [ "$purge" -eq 1 ]; then
        [ "$mode" = user ] || die "--purge removes per-user data; run it without --system as each user"
        rm -rf -- "$config_dir" "$cache_dir"
        say "removed $config_dir and $cache_dir"
    else
        say "Kept your settings and download cache: $config_dir, $cache_dir"
        say "(remove them with --uninstall --purge)."
    fi
    say "The API key in your system keyring is kept. To remove it:"
    say "  secret-tool clear application $APP_ID kind api-key"
    exit 0
fi

# --- Obtain the binary ---------------------------------------------------------

if [ -x "$SRC_DIR/bin/$BIN_NAME" ]; then
    built=$SRC_DIR/bin/$BIN_NAME
    say "Using the prebuilt binary in $SRC_DIR/bin."
elif [ -f "$SRC_DIR/Cargo.toml" ]; then
    [ "$(id -u)" -ne 0 ] || die "refusing to build as root (it would leave root-owned files in target/). Run without sudo; --system asks for sudo only to copy files."
    if ! command -v cargo >/dev/null 2>&1; then
        say "cargo was not found. Install the build dependencies:"
        hint_build
        exit 1
    fi
    if command -v pkg-config >/dev/null 2>&1; then
        if ! pkg-config --atleast-version=4.14 gtk4 || ! pkg-config --atleast-version=1.5 libadwaita-1; then
            say "GTK 4 >= 4.14 and libadwaita >= 1.5 development files are required:"
            hint_build
            exit 1
        fi
    else
        warn "pkg-config not found; the build will fail if GTK development files are missing"
    fi
    say "Building $BIN_NAME (release). The first build takes a few minutes..."
    (cd "$SRC_DIR" && cargo build --release --locked)
    built=$SRC_DIR/target/release/$BIN_NAME
else
    die "no bin/$BIN_NAME and no Cargo.toml next to this script"
fi

# Missing shared libraries mean the runtime packages are absent.
if command -v ldd >/dev/null 2>&1 && ldd "$built" 2>/dev/null | grep -q 'not found'; then
    say "Some libraries Filester Manager needs are missing:"
    ldd "$built" | grep 'not found' | sed 's/^/  /'
    say "Install them with:"
    hint_runtime
    exit 1
fi

for f in "$SRC_DIR/data/$APP_ID.desktop" "$SRC_DIR/data/$APP_ID.metainfo.xml" \
    "$SRC_DIR/data/icons/hicolor/scalable/apps/$APP_ID.svg"; do
    [ -f "$f" ] || die "missing $f"
done

# --- Install -----------------------------------------------------------------

# The desktop entry gets the absolute binary path, so launching from the
# menu works even when the bin directory is not on the session's PATH.
# Desktop-entry quoting: wrap in double quotes, escape \ " ` $.
tmp_desktop=$(mktemp)
trap 'rm -f "$tmp_desktop"' EXIT
exec_quoted=$(printf '%s' "$bin_path" | sed 's/[\\"`$]/\\&/g')
EXEC_LINE="\"$exec_quoted\"" awk '
    /^Exec=/    { print "Exec=" ENVIRON["EXEC_LINE"]; next }
    /^TryExec=/ { next }
    { print }
' "$SRC_DIR/data/$APP_ID.desktop" >"$tmp_desktop"

say "Installing to $bin_dir and $share_dir ..."
as_target install -Dm755 "$built" "$bin_path"
as_target install -Dm644 "$tmp_desktop" "$desktop_path"
as_target install -Dm644 "$SRC_DIR/data/$APP_ID.metainfo.xml" "$metainfo_path"
as_target install -Dm644 "$SRC_DIR/data/icons/hicolor/scalable/apps/$APP_ID.svg" "$icon_path"
refresh_caches

say ""
say "Filester Manager is installed. Open it from your application menu, or run:"
say "  $bin_path"
if [ "$mode" = user ]; then
    case ":${PATH:-}:" in
        *":$bin_dir:"*) ;;
        *) say "($bin_dir is not on your PATH, so typing '$BIN_NAME' alone won't work in a terminal. The menu entry works regardless.)" ;;
    esac
fi
case $mode in
    user) again= ;;
    system) again=' --system' ;;
    prefix) again=" --prefix '$prefix'" ;;
esac
say "Uninstall later with: $0 --uninstall$again"

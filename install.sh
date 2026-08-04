#!/bin/sh
# Build and install tio-chat (ee2e-chat).
#
#   ./install.sh                 install to ~/.local/bin
#   ./install.sh --prefix /usr   install to /usr/bin (needs write access)
#   ./install.sh --uninstall     remove it again
#
# Installs under $HOME by default, so it never needs root.

set -eu

BIN=ee2e-chat
PREFIX="${PREFIX:-$HOME/.local}"
REPO="${EE2E_REPO:-}"
UNINSTALL=0

# Colour only when writing to a terminal, so piping to a file stays readable.
if [ -t 1 ]; then
    R=$(printf '\033[0m') B=$(printf '\033[1m')
    G=$(printf '\033[32m') Y=$(printf '\033[33m') E=$(printf '\033[31m')
else
    R='' B='' G='' Y='' E=''
fi

say()  { printf '%s\n' "$*"; }
step() { printf '%s==>%s %s\n' "$G$B" "$R" "$*"; }
warn() { printf '%s warning:%s %s\n' "$Y$B" "$R" "$*" >&2; }
die()  { printf '%serror:%s %s\n' "$E$B" "$R" "$*" >&2; exit 1; }

usage() {
    cat <<EOF
Usage: install.sh [options]

  --prefix DIR   install under DIR (default: \$HOME/.local)
  --uninstall    remove an installed copy
  -h, --help     this message

Environment:
  PREFIX         same as --prefix
  EE2E_REPO      git URL to clone when run outside a checkout
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; PREFIX="$2"; shift 2 ;;
        --prefix=*) PREFIX="${1#*=}"; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1  (try --help)" ;;
    esac
done

DEST="$PREFIX/bin"
TARGET="$DEST/$BIN"

if [ "$UNINSTALL" -eq 1 ]; then
    if [ -e "$TARGET" ]; then
        rm -f "$TARGET"
        step "removed $TARGET"
    else
        say "nothing installed at $TARGET"
    fi
    # The identity is deliberately left alone: deleting it would discard the
    # key others have verified, and it is not this script's to throw away.
    if [ -e "${XDG_CONFIG_HOME:-$HOME/.config}/ee2e-chat/identity" ]; then
        say ""
        say "Your identity was kept at ${XDG_CONFIG_HOME:-$HOME/.config}/ee2e-chat/identity"
        say "Delete it yourself if you want a new one."
    fi
    exit 0
fi

# --- find the source ---------------------------------------------------------

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" 2>/dev/null && pwd || echo '')
CLEANUP=''
# shellcheck disable=SC2064
trap 'if [ -n "$CLEANUP" ]; then rm -rf "$CLEANUP"; fi' EXIT INT TERM

if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/Cargo.toml" ]; then
    SRC="$SCRIPT_DIR"
    step "building from $SRC"
else
    # Piped from curl, so there is no checkout to build.
    [ -n "$REPO" ] || die "run this from a checkout, or set EE2E_REPO to a git URL"
    command -v git >/dev/null 2>&1 || die "git is required to clone $REPO"
    CLEANUP=$(mktemp -d)
    SRC="$CLEANUP/src"
    step "cloning $REPO"
    git clone --depth 1 "$REPO" "$SRC" >/dev/null 2>&1 || die "could not clone $REPO"
fi

# --- toolchain ---------------------------------------------------------------

if ! command -v cargo >/dev/null 2>&1; then
    # rustup may be installed without its environment having been sourced in
    # this shell, which looks identical to it being absent.
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env"
    fi
fi

if ! command -v cargo >/dev/null 2>&1; then
    say ""
    say "${B}Rust is not installed.${R} tio-chat is built from source, so it is needed."
    say ""
    say "  Arch:      sudo pacman -S rust"
    say "  Debian:    sudo apt install cargo"
    say "  Anywhere:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    say ""
    die "install Rust, then run this again"
fi

# --- build -------------------------------------------------------------------

step "compiling (this takes a minute the first time)"
( cd "$SRC" && cargo build --release --quiet ) || die "the build failed"

BUILT="$SRC/target/release/$BIN"
[ -x "$BUILT" ] || die "expected a binary at $BUILT but found none"

# --- install -----------------------------------------------------------------

mkdir -p "$DEST" || die "could not create $DEST"
[ -w "$DEST" ] || die "$DEST is not writable; try --prefix \"\$HOME/.local\""

# Installed to a temporary name and moved into place, so an existing copy is
# never left half-written if this is interrupted mid-write.
install -m 755 "$BUILT" "$TARGET.new" || die "could not write to $DEST"
mv -f "$TARGET.new" "$TARGET"

step "installed $TARGET"

# --- PATH --------------------------------------------------------------------

case ":$PATH:" in
    *":$DEST:"*) ON_PATH=1 ;;
    *) ON_PATH=0 ;;
esac

say ""
if [ "$ON_PATH" -eq 1 ]; then
    say "Run it with:  ${B}$BIN${R}"
else
    warn "$DEST is not on your PATH"
    say ""
    say "Add it by appending this to ~/.bashrc (or ~/.zshrc):"
    say ""
    say "    export PATH=\"$DEST:\$PATH\""
    say ""
    say "Until then, run it with:  ${B}$TARGET${R}"
fi

say ""
say "Start it with no arguments and it will ask for your name and who to"
say "connect to. Or skip the prompt:"
say ""
say "    $BIN --name alice --listen 0.0.0.0:9001"
say "    $BIN --name bob --listen 0.0.0.0:9002 --connect 192.168.1.42:9001"
say ""

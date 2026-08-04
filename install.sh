#!/bin/sh
# Build and install TIO CHAT. The installed command is `chat`.
#
#   ./install.sh                 install to ~/.local/bin
#   ./install.sh --with-rust     install Rust too, if it is missing
#   ./install.sh --prefix /usr   install to /usr/bin (needs write access)
#   ./install.sh --uninstall     remove it again
#
# Installs under $HOME by default, so it never needs root.

set -eu

BIN=chat
PREFIX="${PREFIX:-$HOME/.local}"
# Overridable so a fork can be installed without editing this file.
REPO="${EE2E_REPO:-https://github.com/tjommilaskus/ee2e-chat.git}"
UNINSTALL=0
WITH_RUST=0
RUST_WAS_INSTALLED=0

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
  --with-rust    install Rust via rustup if missing, without asking first
  --uninstall    remove an installed copy
  -h, --help     this message

Environment:
  PREFIX         same as --prefix
  EE2E_REPO      git URL to clone when run outside a checkout

Rust is the only requirement. If it is missing you are offered rustup, which
installs under your home directory and needs no root.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; PREFIX="$2"; shift 2 ;;
        --prefix=*) PREFIX="${1#*=}"; shift ;;
        --with-rust|-y) WITH_RUST=1; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1  (try --help)" ;;
    esac
done

DEST="$PREFIX/bin"
TARGET="$DEST/$BIN"

# First match for a name in PATH order.
#
# Run in a subshell so the IFS change is contained, and searched by hand rather
# than with `command -v`, which may answer from a hash table built before this
# script created anything.
find_on_path() (
    IFS=:
    for dir in $PATH; do
        [ -n "$dir" ] || dir=.
        if [ -f "$dir/$1" ] && [ -x "$dir/$1" ]; then
            printf '%s\n' "$dir/$1"
            return 0
        fi
    done
    return 1
)

# Which package a file belongs to, where the system can say.
owner_of() {
    if command -v pacman >/dev/null 2>&1; then
        pacman -Qo "$1" 2>/dev/null | sed -n 's/.* is owned by \(.*\)/\1/p'
    elif command -v dpkg >/dev/null 2>&1; then
        dpkg -S "$1" 2>/dev/null | cut -d: -f1
    fi
}

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
    # Piped from curl, so there is no checkout to build; fetch one.
    [ -n "$REPO" ] || die "run this from a checkout, or set EE2E_REPO to a git URL"
    command -v git >/dev/null 2>&1 || die "git is needed to fetch the source -- install it, or clone the repository yourself and run this script from inside it"
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
    say "${B}Rust is not installed.${R} This is built from source, so it is needed."
    say ""

    # Opened rather than merely tested for: /dev/tty exists even with no
    # controlling terminal, and only opening it reveals that. Testing with
    # `[ -r ]` prints the question and then fails to read an answer.
    if [ "$WITH_RUST" -eq 1 ]; then
        answer=y
    elif { true < /dev/tty; } 2>/dev/null; then
        # Read from the terminal, not stdin. Under `curl | sh` stdin is the
        # script itself, and reading it here would swallow the rest of this
        # file instead of an answer.
        printf '%sInstall it now with rustup? It installs under your home directory and needs no root. [y/N] %s' "$B" "$R"
        read -r answer < /dev/tty || answer=n
        say ""
    else
        answer=n
    fi

    case "$answer" in
        y|Y|yes|YES)
            command -v curl >/dev/null 2>&1 || die "curl is needed to fetch rustup"
            step "installing Rust via rustup"
            # -y because we already asked; rustup would otherwise prompt on a
            # terminal we may not have.
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
                | sh -s -- -y --no-modify-path >/dev/null 2>&1 \
                || die "rustup failed; install Rust yourself and run this again"

            # rustup only edits shell profiles, which do not affect a shell
            # already running, so the environment is applied by hand.
            if [ -f "$HOME/.cargo/env" ]; then
                # shellcheck disable=SC1091
                . "$HOME/.cargo/env"
            fi
            command -v cargo >/dev/null 2>&1 || die "Rust installed but cargo is still not on PATH"
            step "Rust installed"
            RUST_WAS_INSTALLED=1
            ;;
        *)
            say "  Arch:      sudo pacman -S rust"
            say "  Debian:    sudo apt install cargo"
            say "  Fedora:    sudo dnf install cargo"
            say "  macOS:     brew install rust"
            say "  Anywhere:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
            say ""
            say "Or re-run this with --with-rust to have it done for you."
            say ""
            die "install Rust, then run this again"
            ;;
    esac
fi

# --- build -------------------------------------------------------------------

step "compiling (this takes a minute the first time)"
( cd "$SRC" && cargo build --release --quiet ) || die "the build failed"

BUILT="$SRC/target/release/$BIN"
[ -x "$BUILT" ] || die "expected a binary at $BUILT but found none"

# --- install -----------------------------------------------------------------

# Noted before installing, so our own copy cannot be mistaken for somebody
# else's. `chat` is a common enough name to already be taken -- ppp ships one.
PREEXISTING=$(find_on_path "$BIN" || true)
[ "$PREEXISTING" = "$TARGET" ] && PREEXISTING=''

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

# --- name clash ---------------------------------------------------------------

if [ -n "$PREEXISTING" ]; then
    OWNER=$(owner_of "$PREEXISTING")
    [ -n "$OWNER" ] && OWNER=" (from $OWNER)"
    WINNER=$(find_on_path "$BIN" || true)

    say ""
    if [ "$WINNER" = "$TARGET" ]; then
        warn "there was already a ${B}$BIN${R} at $PREEXISTING$OWNER"
        say ""
        say "Yours comes first in PATH, so typing ${B}$BIN${R} now runs this one."
        say "The other is still there, reachable as $PREEXISTING"
    else
        # The more awkward way round: installed, but typing the name runs
        # something else, which would otherwise look like a broken install.
        warn "$PREEXISTING$OWNER comes before yours in PATH"
        say ""
        say "Typing ${B}$BIN${R} will run that, not the one just installed."
        say "Put $DEST earlier in your PATH, or run it as $TARGET"
    fi
fi

if [ "$RUST_WAS_INSTALLED" -eq 1 ]; then
    say ""
    say "Rust was installed for you. To use cargo in future shells, add:"
    say ""
    say "    . \"\$HOME/.cargo/env\""
    say ""
    say "to your ~/.bashrc, or just open a new terminal."
fi

say ""
say "Start it with no arguments and it will ask for your name and who to"
say "connect to. Or skip the prompt:"
say ""
say "    $BIN --name alice --listen 0.0.0.0:9001"
say "    $BIN --name bob --listen 0.0.0.0:9002 --connect 192.168.1.42:9001"
say ""
say "Later, ${B}$BIN update${R} fetches and installs the latest version."
say ""

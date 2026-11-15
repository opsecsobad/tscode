#!/usr/bin/env bash
# Build the Rust version of tscode and make it the app.
#
# The built app always lives in ./app, whichever version it is. Running this script
# deletes whatever is there (e.g. the Python build) and replaces it with the Rust
# binary, then points the `tscode` command at it.
set -euo pipefail

ROOT="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"
BIN_DIR="${TSCODE_BIN_DIR:-$HOME/.local/bin}"

command -v cargo >/dev/null || {
    echo "build-rust: cargo not found. Install Rust from https://rustup.rs and run this again." >&2
    exit 1
}

# Compile outside the repo so no build files end up in it.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/tscode/rust-target}"
echo "Building the Rust version (the first build takes a few minutes)..."
cargo build --release --locked --manifest-path "$ROOT/rust/Cargo.toml"

# The build worked: swap the app. A failed build leaves the old app untouched.
rm -rf "$ROOT/app"
mkdir -p "$ROOT/app"
cp "$CARGO_TARGET_DIR/release/tscode" "$ROOT/app/tscode"

mkdir -p "$BIN_DIR"
if [ -L "$BIN_DIR/tscode" ] || [ ! -e "$BIN_DIR/tscode" ]; then
    ln -sfn "$ROOT/app/tscode" "$BIN_DIR/tscode"
    echo "Done. The Rust version is now the app: $ROOT/app/tscode (command: tscode)"
else
    echo "Done. The Rust version is now the app: $ROOT/app/tscode"
    echo "Note: $BIN_DIR/tscode is a regular file, so it was left alone. Remove it to have the command linked."
fi

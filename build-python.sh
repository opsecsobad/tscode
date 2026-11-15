#!/usr/bin/env bash
# Build the Python version of tscode and make it the app.
#
# The built app always lives in ./app, whichever version it is. Running this script
# deletes whatever is there (e.g. the Rust binary) and replaces it with the Python
# build (a wheel plus a launcher), then points the `tscode` command at it.
set -euo pipefail

ROOT="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"
BIN_DIR="${TSCODE_BIN_DIR:-$HOME/.local/bin}"

command -v uv >/dev/null || {
    echo "build-python: uv not found. Install it from https://docs.astral.sh/uv/ and run this again." >&2
    exit 1
}

# Build into a scratch folder so no build files end up in the repo.
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT
echo "Building the Python version..."
uv build --quiet --wheel --out-dir "$OUT" "$ROOT/python"
WHEEL="$(basename "$(ls "$OUT"/tscode-*.whl)")"

# The build worked: swap the app. A failed build leaves the old app untouched.
rm -rf "$ROOT/app"
mkdir -p "$ROOT/app"
cp "$OUT/$WHEEL" "$ROOT/app/$WHEEL"
cat > "$ROOT/app/tscode" <<EOF
#!/usr/bin/env bash
# Launch Terminal Studio Code (Python build). Usage: tscode [folder-or-file]
# Works from anywhere (and through a symlink). uv keeps the app's environment in its cache.
DIR="\$(dirname "\$(readlink -f "\${BASH_SOURCE[0]}")")"
exec uv tool run --quiet --from "\$DIR/$WHEEL" tscode "\$@"
EOF
chmod +x "$ROOT/app/tscode"

# First run installs the dependencies into uv's cache, so launching is quick afterwards.
echo "Preparing its environment..."
"$ROOT/app/tscode" --list-themes >/dev/null

mkdir -p "$BIN_DIR"
if [ -L "$BIN_DIR/tscode" ] || [ ! -e "$BIN_DIR/tscode" ]; then
    ln -sfn "$ROOT/app/tscode" "$BIN_DIR/tscode"
    echo "Done. The Python version is now the app: $ROOT/app/tscode (command: tscode)"
else
    echo "Done. The Python version is now the app: $ROOT/app/tscode"
    echo "Note: $BIN_DIR/tscode is a regular file, so it was left alone. Remove it to have the command linked."
fi

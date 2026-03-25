#!/usr/bin/env bash
set -euo pipefail

# Usage: scripts/build-tar.sh <version> <target-dir> [<arch>] [<os>]
# Produces: dist/omnish-<version>-<os>-<arch>.tar.gz

VERSION="${1:?Usage: build-tar.sh <version> <target-dir> [<arch>] [<os>]}"
TARGET_DIR="${2:?Usage: build-tar.sh <version> <target-dir> [<arch>] [<os>]}"
ARCH="${3:-x86_64}"
OS="${4:-$(uname -s | tr '[:upper:]' '[:lower:]')}"
[[ "$OS" == "darwin" ]] && OS="macos"

# Locate repo root (one level up from scripts/)
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

STAGING="dist/omnish-${VERSION}-${OS}-${ARCH}"
rm -rf "$STAGING"
mkdir -p "$STAGING/bin"

# Copy binaries (omnish-daemon may not exist for client-only platforms)
cp "$TARGET_DIR/omnish"        "$STAGING/bin/"
cp "$TARGET_DIR/omnish-plugin" "$STAGING/bin/"
if [[ -f "$TARGET_DIR/omnish-daemon" ]]; then
    cp "$TARGET_DIR/omnish-daemon" "$STAGING/bin/"
fi

# Ad-hoc code signing on macOS
if [[ "$OS" == "macos" ]]; then
    for bin in "$STAGING/bin/"*; do
        codesign -s - "$bin"
    done
    echo "Code-signed macOS binaries"
fi

# Assets: plugin configs, chat prompts, update script
mkdir -p "$STAGING/assets/plugins/builtin" "$STAGING/assets/prompts"

cp "$REPO_ROOT/crates/omnish-plugin/assets/tool.json"                    "$STAGING/assets/plugins/builtin/"
cp "$REPO_ROOT/crates/omnish-plugin/assets/tool.override.json.example"   "$STAGING/assets/plugins/builtin/"
cp "$REPO_ROOT/crates/omnish-llm/assets/chat.json"                       "$STAGING/assets/prompts/"
cp "$REPO_ROOT/crates/omnish-llm/assets/chat.override.json.example"      "$STAGING/assets/prompts/"
cp "$REPO_ROOT/install.sh"                                               "$STAGING/"
cp "$REPO_ROOT/scripts/deploy.sh"                                        "$STAGING/assets/"

# Plugins
if [[ -d "$REPO_ROOT/plugins" ]]; then
    cp -r "$REPO_ROOT/plugins" "$STAGING/"
fi

tar -czf "${STAGING}.tar.gz" -C dist "omnish-${VERSION}-${OS}-${ARCH}"
echo "Created ${STAGING}.tar.gz"

#!/usr/bin/env bash
set -euo pipefail

# Usage: scripts/build-tar.sh <version> <target-dir>
# Produces: dist/omnish-<version>-linux-x86_64.tar.gz

VERSION="${1:?Usage: build-tar.sh <version> <target-dir>}"
TARGET_DIR="${2:?Usage: build-tar.sh <version> <target-dir>}"

# Locate repo root (one level up from scripts/)
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

STAGING="dist/omnish-${VERSION}-linux-x86_64"
rm -rf "$STAGING"
mkdir -p "$STAGING/bin"

cp "$TARGET_DIR/omnish"        "$STAGING/bin/"
cp "$TARGET_DIR/omnish-daemon" "$STAGING/bin/"
cp "$TARGET_DIR/omnish-plugin" "$STAGING/bin/"

# Assets: plugin configs, chat prompts, update script
mkdir -p "$STAGING/assets/plugins/builtin" "$STAGING/assets/prompts"

cp "$REPO_ROOT/crates/omnish-plugin/assets/tool.json"                    "$STAGING/assets/plugins/builtin/"
cp "$REPO_ROOT/crates/omnish-plugin/assets/tool.override.json.example"   "$STAGING/assets/plugins/builtin/"
cp "$REPO_ROOT/crates/omnish-llm/assets/chat.json"                       "$STAGING/assets/prompts/"
cp "$REPO_ROOT/crates/omnish-llm/assets/chat.override.json.example"      "$STAGING/assets/prompts/"
cp "$REPO_ROOT/scripts/update.sh"                                        "$STAGING/assets/"

tar -czf "${STAGING}.tar.gz" -C dist "omnish-${VERSION}-linux-x86_64"
echo "Created ${STAGING}.tar.gz"

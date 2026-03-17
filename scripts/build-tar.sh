#!/usr/bin/env bash
set -euo pipefail

# Usage: scripts/build-tar.sh <version> <target-dir>
# Produces: dist/omnish-<version>-linux-x86_64.tar.gz

VERSION="${1:?Usage: build-tar.sh <version> <target-dir>}"
TARGET_DIR="${2:?Usage: build-tar.sh <version> <target-dir>}"

STAGING="dist/omnish-${VERSION}-linux-x86_64"
rm -rf "$STAGING"
mkdir -p "$STAGING/bin"

cp "$TARGET_DIR/omnish"        "$STAGING/bin/"
cp "$TARGET_DIR/omnish-daemon" "$STAGING/bin/"
cp "$TARGET_DIR/omnish-plugin" "$STAGING/bin/"

# plugins directory (placeholder for external plugins)
mkdir -p "$STAGING/plugins"

tar -czf "${STAGING}.tar.gz" -C dist "omnish-${VERSION}-linux-x86_64"
echo "Created ${STAGING}.tar.gz"

#!/usr/bin/env bash
set -euo pipefail

# Local build script - replicates CI packaging on a dev machine.
#
# Usage:
#   scripts/build-local.sh            # build for current platform
#   scripts/build-local.sh --musl     # build static musl binary (like CI)
#
# Version: derived from git describe.  If HEAD is on a tag → that tag version.
# Otherwise → <tag>.N where N is commits after the tag (e.g. 0.8.1.21).
#
# Output: dist/omnish-<version>-linux-<arch>.tar.gz

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# ── Parse args ──────────────────────────────────────────────────────────────

USE_MUSL=false
for arg in "$@"; do
    case "$arg" in
        --musl) USE_MUSL=true ;;
        -h|--help)
            echo "Usage: scripts/build-local.sh [--musl]"
            echo ""
            echo "  --musl    Build static musl binary (requires musl-gcc)"
            echo "  Default:  Build with native toolchain"
            exit 0
            ;;
        *) echo "Unknown option: $arg" >&2; exit 1 ;;
    esac
done

# ── Version ─────────────────────────────────────────────────────────────────

GIT_DESC=$(git describe --tags --always 2>/dev/null || echo "")
if [[ -z "$GIT_DESC" ]]; then
    # No tags at all - use Cargo.toml version
    VERSION=$(cargo metadata --no-deps --format-version 1 \
        | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])")
elif [[ "$GIT_DESC" =~ ^v?([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
    # Exactly on a tag
    VERSION="${BASH_REMATCH[1]}"
elif [[ "$GIT_DESC" =~ ^v?([0-9]+\.[0-9]+\.[0-9]+)-([0-9]+)-g ]]; then
    # N commits after tag: 0.8.1-21-gabcdef → 0.8.1.21
    VERSION="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}"
else
    VERSION="$GIT_DESC"
fi

echo "Version: $VERSION"

# ── Build ───────────────────────────────────────────────────────────────────

ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
esac

if [[ "$USE_MUSL" == true ]]; then
    TARGET="x86_64-unknown-linux-musl"
    echo "Building for $TARGET..."
    cargo build --release --target "$TARGET"
    TARGET_DIR="target/$TARGET/release"
else
    echo "Building (native)..."
    cargo build --release
    TARGET_DIR="target/release"
fi

# ── Platform ───────────────────────────────────────────────────────────────

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
[[ "$OS" == "darwin" ]] && OS="macos"

# ── Package (delegate to build-tar.sh) ──────────────────────────────────────

bash "$REPO_ROOT/scripts/build-tar.sh" "$VERSION" "$TARGET_DIR" "$ARCH" "$OS"
echo ""
echo "To install: bash install.sh --dir=dist"

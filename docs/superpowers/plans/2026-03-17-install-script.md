# Install Script Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create an `install.sh` that downloads omnish from GitHub/GitLab, sets up the server (daemon.toml, credentials), and outputs client deployment instructions.

**Architecture:** A single shell script with two phases: (1) download & extract tar to `~/.omnish/`, (2) interactive setup wizard that configures LLM backends, listen address, generates credentials via `omnish-daemon --init-credentials`, and prints client deployment commands. CI pipeline updated to produce the tar artifact.

**Tech Stack:** Bash, GitLab CI, `curl`/`wget`, `tar`, `scp` (suggested to user)

---

## File Structure

- **Create:** `install.sh` - main install script (root of repo)
- **Create:** `scripts/build-tar.sh` - helper to build the tar archive in CI
- **Modify:** `.gitlab-ci.yml` - update build-release to produce tar.gz
- **Modify:** `crates/omnish-daemon/src/main.rs` - add `--init-credentials` flag

---

## Chunk 1: Daemon `--init-credentials` flag

### Task 1: Add `--init-credentials` to daemon

**Files:**
- Modify: `crates/omnish-daemon/src/main.rs:15-19`

- [ ] **Step 1: Add the --init-credentials branch**

In `main()`, after the `--version` check, add:

```rust
if std::env::args().any(|a| a == "--init-credentials") {
    let omnish_dir = omnish_dir();
    std::fs::create_dir_all(&omnish_dir)?;

    // Generate auth token
    let token_path = omnish_common::auth::default_token_path();
    let token = omnish_common::auth::load_or_create_token(&token_path)?;
    println!("auth_token: {} ({})", token_path.display(),
        if token_path.exists() { "existing" } else { "created" });

    // Generate TLS cert
    let tls_dir = omnish_transport::tls::default_tls_dir();
    let _ = omnish_transport::tls::load_or_create_cert(&tls_dir)?;
    println!("tls cert:   {}/cert.pem", tls_dir.display());
    println!("tls key:    {}/key.pem", tls_dir.display());

    return Ok(());
}
```

- [ ] **Step 2: Build and test**

Run: `cargo build -p omnish-daemon`
Then: `./target/debug/omnish-daemon --init-credentials`
Expected: prints paths, creates `~/.omnish/auth_token` and `~/.omnish/tls/{cert,key}.pem`

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-daemon/src/main.rs
git commit -m "feat: add --init-credentials flag to daemon"
```

---

## Chunk 2: CI tar archive

### Task 2: Build tar.gz in CI

**Files:**
- Create: `scripts/build-tar.sh`
- Modify: `.gitlab-ci.yml:29-51`

- [ ] **Step 1: Create build-tar.sh**

```bash
#!/usr/bin/env bash
set -euo pipefail

# Usage: scripts/build-tar.sh <version> <target-dir>
# Produces: dist/omnish-v<version>-linux-x86_64.tar.gz

VERSION="${1:?Usage: build-tar.sh <version> <target-dir>}"
TARGET_DIR="${2:?Usage: build-tar.sh <version> <target-dir>}"

STAGING="dist/omnish-v${VERSION}-linux-x86_64"
rm -rf "$STAGING"
mkdir -p "$STAGING/bin"

cp "$TARGET_DIR/omnish"        "$STAGING/bin/"
cp "$TARGET_DIR/omnish-daemon" "$STAGING/bin/"
cp "$TARGET_DIR/omnish-plugin" "$STAGING/bin/"

# plugins directory (empty placeholder for external plugins)
mkdir -p "$STAGING/plugins"

tar -czf "${STAGING}.tar.gz" -C dist "omnish-v${VERSION}-linux-x86_64"
echo "Created ${STAGING}.tar.gz"
```

- [ ] **Step 2: Update .gitlab-ci.yml build-release job**

Replace the current `script:` and `artifacts:` sections of `build-release`:

```yaml
build-release:
  stage: build
  image: docker.nv/ubuntu_2404_runner:latest
  variables:
    CC_x86_64_unknown_linux_musl: musl-gcc
  cache:
    key: ${CI_COMMIT_REF_SLUG}-musl
    paths:
      - .cargo/registry
      - .cargo/git
      - target/
  script:
    - cargo build --release --target x86_64-unknown-linux-musl
    - VERSION=$(cargo metadata --no-deps --format-version 1 | python3 -c "import sys,json; print(json.load(sys.stdin)['packages'][0]['version'])")
    - bash scripts/build-tar.sh "$VERSION" target/x86_64-unknown-linux-musl/release
    - mkdir -p dist-compat
    - cp target/x86_64-unknown-linux-musl/release/omnish dist-compat/
    - cp target/x86_64-unknown-linux-musl/release/omnish-daemon dist-compat/
    - cp target/x86_64-unknown-linux-musl/release/omnish-plugin dist-compat/
  artifacts:
    paths:
      - dist/*.tar.gz
      - dist-compat/omnish
      - dist-compat/omnish-daemon
      - dist-compat/omnish-plugin
    expire_in: 30 days
  rules:
    - if: $CI_COMMIT_TAG
```

Update the `release:` job asset links to include the tar:

```yaml
release:
  stage: release
  image: docker.nv/ubuntu_2404_runner:latest
  needs:
    - job: build-release
      artifacts: true
  script:
    - echo "Creating release for ${CI_COMMIT_TAG}"
  release:
    tag_name: ${CI_COMMIT_TAG}
    name: "omnish ${CI_COMMIT_TAG}"
    description: "Release ${CI_COMMIT_TAG}"
    assets:
      links:
        - name: "omnish-linux-x86_64.tar.gz"
          url: "${CI_PROJECT_URL}/-/jobs/artifacts/${CI_COMMIT_TAG}/raw/dist/omnish-${CI_COMMIT_TAG}-linux-x86_64.tar.gz?job=build-release"
          filepath: "/omnish-linux-x86_64.tar.gz"
        - name: "omnish (linux-x86_64-static)"
          url: "${CI_PROJECT_URL}/-/jobs/artifacts/${CI_COMMIT_TAG}/raw/dist-compat/omnish?job=build-release"
          filepath: "/omnish"
        - name: "omnish-daemon (linux-x86_64-static)"
          url: "${CI_PROJECT_URL}/-/jobs/artifacts/${CI_COMMIT_TAG}/raw/dist-compat/omnish-daemon?job=build-release"
          filepath: "/omnish-daemon"
        - name: "omnish-plugin (linux-x86_64-static)"
          url: "${CI_PROJECT_URL}/-/jobs/artifacts/${CI_COMMIT_TAG}/raw/dist-compat/omnish-plugin?job=build-release"
          filepath: "/omnish-plugin"
  rules:
    - if: $CI_COMMIT_TAG
```

- [ ] **Step 3: Commit**

```bash
git add scripts/build-tar.sh .gitlab-ci.yml
git commit -m "feat: CI produces tar.gz release artifact"
```

---

## Chunk 3: Install script

### Task 3: Create install.sh

**Files:**
- Create: `install.sh`

- [ ] **Step 1: Write install.sh**

The script has these sections:

**Header & helpers:**
```bash
#!/usr/bin/env bash
set -euo pipefail

OMNISH_DIR="${HOME}/.omnish"
BIN_DIR="${OMNISH_DIR}/bin"

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mWARN:\033[0m %s\n' "$*"; }
error() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }
ask()   { printf '\033[1;32m?\033[0m %s ' "$1"; read -r REPLY; }
```

**Platform detection:**
```bash
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) error "Unsupported architecture: $ARCH" ;;
esac
[[ "$OS" == "linux" ]] || error "Only Linux is supported currently"
```

**Download source selection:**
```bash
# Parse args
SOURCE="gitlab"
FORCE=false
VERSION=""
for arg in "$@"; do
    case "$arg" in
        --github)  SOURCE="github" ;;
        --gitlab)  SOURCE="gitlab" ;;
        --force)   FORCE=true ;;
        --version=*) VERSION="${arg#*=}" ;;
    esac
done
```

**Download & extract:**
```bash
info "Downloading omnish from $SOURCE..."

if [[ "$SOURCE" == "github" ]]; then
    REPO="yrlihuan/omnish"
    if [[ -z "$VERSION" ]]; then
        VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed 's/.*"tag_name": *"//;s/".*//')
    fi
    TAR_URL="https://github.com/${REPO}/releases/download/${VERSION}/omnish-${VERSION}-linux-${ARCH}.tar.gz"
else
    # GitLab: use glab or direct artifact URL
    PROJECT="dev%2Fomnish"
    if [[ -z "$VERSION" ]]; then
        VERSION=$(glab api "projects/${PROJECT}/releases" 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)[0]['tag_name'])" 2>/dev/null || echo "")
        [[ -n "$VERSION" ]] || error "Could not determine latest version. Use --version=vX.Y.Z"
    fi
    TAR_URL=$(glab api "projects/${PROJECT}/releases/${VERSION}" 2>/dev/null | python3 -c "
import sys, json
links = json.load(sys.stdin).get('assets', {}).get('links', [])
for l in links:
    if 'tar.gz' in l.get('name', ''):
        print(l['direct_asset_url'])
        break
" 2>/dev/null || echo "")
    [[ -n "$TAR_URL" ]] || error "Could not find tar.gz asset for ${VERSION}"
fi

info "Version: $VERSION"

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

curl -fSL "$TAR_URL" -o "$TMPDIR/omnish.tar.gz" || error "Download failed"
tar -xzf "$TMPDIR/omnish.tar.gz" -C "$TMPDIR"

# Find extracted directory
EXTRACTED=$(find "$TMPDIR" -maxdepth 1 -type d -name 'omnish-*' | head -1)
[[ -d "$EXTRACTED" ]] || error "Unexpected archive layout"
```

**Install to ~/.omnish/:**
```bash
info "Installing to ${OMNISH_DIR}..."

mkdir -p "$BIN_DIR" "$OMNISH_DIR/plugins"

# Copy binaries
cp "$EXTRACTED/bin/"* "$BIN_DIR/"
chmod 755 "$BIN_DIR"/*

# Copy plugins if present
if [[ -d "$EXTRACTED/plugins" ]] && ls "$EXTRACTED/plugins/"* &>/dev/null; then
    cp -r "$EXTRACTED/plugins/"* "$OMNISH_DIR/plugins/"
fi

# Set directory permissions
chmod 700 "$OMNISH_DIR"
```

**LLM configuration wizard:**
```bash
info "Configuring LLM backends..."

configure_backend() {
    local name="$1"
    local purpose="$2"

    echo ""
    info "Configure $purpose backend ($name):"
    echo "  [1] Anthropic"
    echo "  [2] OpenAI-compatible"
    ask "Backend type [1]:"
    local btype="${REPLY:-1}"
    case "$btype" in
        1) BACKEND_TYPE="anthropic" ;;
        2) BACKEND_TYPE="openai" ;;
        *) BACKEND_TYPE="anthropic" ;;
    esac

    ask "Model name (e.g. claude-sonnet-4-20250514):"
    local model="$REPLY"

    ask "API key:"
    local api_key="$REPLY"

    ask "Base URL (leave empty for default):"
    local base_url="$REPLY"

    # Write TOML section
    echo "[llm.backends.${name}]"
    echo "backend_type = \"${BACKEND_TYPE}\""
    echo "model = \"${model}\""
    echo "api_key_cmd = 'echo \"${api_key}\"'"
    if [[ -n "$base_url" ]]; then
        echo "base_url = \"${base_url}\""
    fi
    echo ""
}

DAEMON_TOML="$OMNISH_DIR/daemon.toml"

if [[ -f "$DAEMON_TOML" ]] && [[ "$FORCE" != true ]]; then
    info "daemon.toml already exists, skipping LLM configuration (use --force to overwrite)"
else
    {
        # Chat/analysis backend
        configure_backend "claude" "chat/analysis"
    } > "$TMPDIR/chat_backend.toml"

    ask "Use the same backend for completion? [Y/n]:"
    SAME="${REPLY:-Y}"

    if [[ "$SAME" =~ ^[Nn] ]]; then
        {
            configure_backend "claude-fast" "completion"
        } > "$TMPDIR/completion_backend.toml"
        USE_CASES='[llm.use_cases]
chat = "claude"
analysis = "claude"
completion = "claude-fast"'
    else
        USE_CASES='[llm.use_cases]
chat = "claude"
analysis = "claude"
completion = "claude"'
    fi

    # Listen address
    echo ""
    info "Daemon listen address:"
    echo "  [1] Unix socket (local only, default)"
    echo "  [2] TCP (for remote clients)"
    ask "Choice [1]:"
    LISTEN_CHOICE="${REPLY:-1}"

    if [[ "$LISTEN_CHOICE" == "2" ]]; then
        ask "TCP address (e.g. 0.0.0.0:9800):"
        LISTEN_ADDR="$REPLY"
    else
        LISTEN_ADDR="${OMNISH_DIR}/omnish.sock"
    fi

    # Assemble daemon.toml
    {
        echo "listen_addr = \"${LISTEN_ADDR}\""
        echo ""
        echo "[llm]"
        echo 'default = "claude"'
        echo ""
        cat "$TMPDIR/chat_backend.toml"
        if [[ -f "$TMPDIR/completion_backend.toml" ]]; then
            cat "$TMPDIR/completion_backend.toml"
        fi
        echo "$USE_CASES"
    } > "$DAEMON_TOML"
    chmod 600 "$DAEMON_TOML"
    info "Written: $DAEMON_TOML"
fi
```

**Generate credentials:**
```bash
info "Generating TLS certificate and auth token..."
"$BIN_DIR/omnish-daemon" --init-credentials
chmod 600 "$OMNISH_DIR/auth_token"
```

**PATH setup:**
```bash
# Check if BIN_DIR is already in PATH
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
    info "Add to your shell profile:"
    echo ""
    echo "  export PATH=\"${BIN_DIR}:\$PATH\""
    echo ""
fi
```

**Client deployment instructions:**
```bash
if [[ "$LISTEN_CHOICE" == "2" ]]; then
    SERVER_IP=$(hostname -I | awk '{print $1}')
    echo ""
    info "=== Client Deployment ==="
    echo ""
    echo "On each client machine, copy these files:"
    echo ""
    echo "  scp -r ${BIN_DIR}/omnish ${BIN_DIR}/omnish-plugin \\"
    echo "      ${OMNISH_DIR}/tls/cert.pem ${OMNISH_DIR}/auth_token \\"
    echo "      user@client:~/.omnish/"
    echo ""
    echo "Then create ~/.omnish/client.toml on the client:"
    echo ""
    echo "  daemon_addr = \"${SERVER_IP}:${LISTEN_ADDR##*:}\""
    echo ""
    echo "And add to PATH:"
    echo ""
    echo "  export PATH=\"\$HOME/.omnish/bin:\$PATH\""
fi

echo ""
info "Installation complete! (omnish $VERSION)"
```

- [ ] **Step 2: Make executable**

```bash
chmod +x install.sh
```

- [ ] **Step 3: Test locally**

Run: `bash install.sh --force`
Verify:
- `~/.omnish/bin/{omnish,omnish-daemon,omnish-plugin}` exist and are executable
- `~/.omnish/daemon.toml` is generated with correct content
- `~/.omnish/auth_token` and `~/.omnish/tls/cert.pem` exist
- Permissions are correct (700 for dir, 600 for sensitive files, 755 for binaries)

- [ ] **Step 4: Commit**

```bash
git add install.sh
git commit -m "feat: add install.sh for automated server deployment"
```

---

## Summary

| Task | What | Files |
|------|------|-------|
| 1 | `--init-credentials` flag | `crates/omnish-daemon/src/main.rs` |
| 2 | CI tar.gz build | `scripts/build-tar.sh`, `.gitlab-ci.yml` |
| 3 | `install.sh` | `install.sh` |

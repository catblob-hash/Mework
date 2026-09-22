#!/usr/bin/env bash
# Provisions a Linux development container for Mework.
#
# Mework ships for Windows: `npm run tauri:build` produces an NSIS installer
# against WebView2, and the formal-verification toolchain in
# `scripts/prob-fetch.mjs` pins Windows artifacts. Neither runs here. What a
# Linux container *can* do is the whole inner development loop — Biome, tsc,
# Vitest, the `node --test` check scripts, and `cargo check` / `cargo test`
# across the entire Rust workspace, the Tauri host crate included — and that is
# what this script sets up. Run it from a fresh clone:
#
#   bash scripts/setup-linux-dev.sh
#
# It is idempotent: every step is skipped when its result is already in place,
# so re-running after a container is recycled costs only the npm install.
#
# Two things need help that a Windows checkout gets for free, and each is a
# step below rather than a note in a README nobody reads:
#
#   * `src-tauri/build.rs` refuses to build without the AI SDK sidecar staged,
#     so the single-file sidecar is built here. (The Linux window icon needs no
#     step: build.rs renders `icons/icon.png` for every non-Windows target.)
#   * The browser-driven tests spawn Chrome from a fixed candidate list with a
#     fixed flag list. A container running as root has no usable Chromium
#     sandbox, so `MEWORK_CHROME_PATH` points at a wrapper that supplies the
#     flags the tests cannot.
#
# `--skip-apt` leaves the system packages alone, for an image that already has
# them or a user without root. `--seed-agent-config` additionally writes the
# untracked agent-client files that `npm test` asserts on; see that step below
# for why it is opt-in.
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
skip_apt=0
seed_agent_config=0
for argument in "$@"; do
  case "$argument" in
    --skip-apt) skip_apt=1 ;;
    --seed-agent-config) seed_agent_config=1 ;;
    *) echo "unknown argument: $argument" >&2; exit 2 ;;
  esac
done

if [ "$(uname -s)" != "Linux" ]; then
  echo "setup-linux-dev.sh targets Linux; on macOS run scripts/setup-macos-dev.sh, on Windows follow README 'Build from source'." >&2
  exit 1
fi

step() { printf '\n[setup] %s\n' "$1"; }

# --- system packages ---------------------------------------------------------
# The GTK/WebKit set is what Tauri 2 links against on Linux. The crate only
# needs them to compile and link its tests here; the shipped app is WebView2.
if [ "$skip_apt" -eq 1 ]; then
  step "system packages: skipped (--skip-apt)"
else
  step "system packages"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y --no-install-recommends \
    build-essential pkg-config perl cmake file patchelf \
    libssl-dev libglib2.0-dev libgtk-3-dev libwebkit2gtk-4.1-dev \
    libsoup-3.0-dev librsvg2-dev librsvg2-bin libayatana-appindicator3-dev

  # GitHub CLI is not in the Ubuntu archive; the vendor repository is.
  if ! command -v gh >/dev/null 2>&1; then
    step "github cli"
    install -d -m 755 /etc/apt/keyrings
    curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /etc/apt/keyrings/githubcli-archive-keyring.gpg
    chmod 644 /etc/apt/keyrings/githubcli-archive-keyring.gpg
    printf 'deb [arch=%s signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main\n' \
      "$(dpkg --print-architecture)" > /etc/apt/sources.list.d/github-cli.list
    apt-get update -qq
    apt-get install -y gh
  fi
fi

if ! command -v cargo >/dev/null 2>&1; then
  step "rust toolchain"
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile default
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi

# --- javascript dependencies -------------------------------------------------
step "npm dependencies (root)"
(cd "$repo_root" && npm ci)
step "npm dependencies (aisdk-service)"
(cd "$repo_root/aisdk-service" && npm ci)

# --- chrome wrapper ----------------------------------------------------------
# scripts/tests/frontend-csp-browser.test.mjs and the browser-dev harness read
# MEWORK_CHROME_PATH first, then a fixed list of standard install locations.
step "chrome wrapper"
chrome_binary=""
for candidate in \
  /opt/pw-browsers/chromium-*/chrome-linux/chrome \
  /usr/bin/google-chrome \
  /usr/bin/chromium \
  /usr/bin/chromium-browser
do
  if [ -x "$candidate" ]; then chrome_binary="$candidate"; break; fi
done
if [ -n "$chrome_binary" ]; then
  install -d -m 755 /opt/mework-dev/bin
  cat > /opt/mework-dev/bin/chrome <<EOF
#!/bin/sh
# Chromium for Mework's browser-driven tests. The tests pass a fixed flag list,
# so the flags a root container needs are added here: without --no-sandbox the
# browser dies before writing DevToolsActivePort, and the tests then time out
# waiting for a DevTools port that never appears.
exec $chrome_binary --no-sandbox --disable-dev-shm-usage --disable-gpu "\$@"
EOF
  chmod +x /opt/mework-dev/bin/chrome
  echo "  wrapper -> $chrome_binary"
else
  echo "  no Chromium found; browser tests will skip or fail until one is installed" >&2
fi

# --- build artifacts the Rust build script requires --------------------------
step "frontend bundle (tauri.conf.json frontendDist)"
(cd "$repo_root" && npm run build >/dev/null)

step "aisdk sidecar"
if [ ! -f "$repo_root/aisdk-service/dist/mework-aisdk" ]; then
  (cd "$repo_root" && npm run build:sidecar)
fi

# --- agent-client configuration ----------------------------------------------
# See scripts/seed-agent-config.sh: by default this only reports what is
# missing, and --seed-agent-config writes the minimum the tests require.
step "agent-client configuration"
if [ "$seed_agent_config" -eq 1 ]; then
  bash "$repo_root/scripts/seed-agent-config.sh" --write
else
  bash "$repo_root/scripts/seed-agent-config.sh"
fi

# --- shell environment -------------------------------------------------------
step "shell environment"
profile=/etc/profile.d/mework-dev.sh
cat > "$profile" <<'EOF'
# Mework development container (scripts/setup-linux-dev.sh).
export MEWORK_CHROME_PATH=/opt/mework-dev/bin/chrome
# Node's built-in fetch ignores HTTPS_PROXY unless asked (Node >= 22.21).
export NODE_USE_ENV_PROXY=1
EOF
chmod 644 "$profile"
echo "  wrote $profile"

step "verifying"
(cd "$repo_root/src-tauri" && cargo check --workspace --all-targets)

cat <<'EOF'

[setup] done. In a new shell (or after `. /etc/profile.d/mework-dev.sh`):

  npm test                    # lint, check scripts, Vitest
  npx vitest run              # frontend unit tests only
  cd src-tauri && cargo test  # Rust workspace

Not available on Linux: `npm run tauri:build` (NSIS/WebView2) and
`npm run prob:fetch` (the ProB toolchain pins Windows artifacts).
EOF

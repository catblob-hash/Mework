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
# Three things need help that a Windows checkout gets for free, and each is a
# step below rather than a note in a README nobody reads:
#
#   * `tauri::generate_context!` opens `src-tauri/icons/icon.png` on non-Windows
#     targets. `src-tauri/build.rs` generates `icon.ico` only — that is the
#     shipped icon — so the PNG is rendered here from the same SVG. It is a
#     local artifact, not a tracked file.
#   * `src-tauri/build.rs` refuses to build without the AI SDK sidecar staged.
#     `aisdk-service/build.mjs` always writes `dist/mework-aisdk.exe`, while
#     `scripts/build-aisdk-sidecar.mjs` and `build.rs` look for the
#     extension-less name off Windows, so the artifact is copied across.
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
  echo "setup-linux-dev.sh targets Linux; on Windows follow README 'Build from source'." >&2
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

step "linux window icon"
rsvg-convert -w 512 -h 512 "$repo_root/src/mework-icon.svg" -o "$repo_root/src-tauri/icons/icon.png"
# Local-only artifact: keep it out of `git status` without touching .gitignore,
# which is shared with the Windows checkouts that never generate this file.
exclude_file="$repo_root/.git/info/exclude"
if [ -f "$exclude_file" ] && ! grep -qxF 'src-tauri/icons/icon.png' "$exclude_file"; then
  echo 'src-tauri/icons/icon.png' >> "$exclude_file"
fi

step "aisdk sidecar"
if [ ! -f "$repo_root/aisdk-service/dist/mework-aisdk" ]; then
  # build.mjs exits non-zero through the wrapper on Linux (it writes the
  # Windows name and the wrapper then cannot find the Unix one), so drive the
  # single-file build directly and place the artifact ourselves.
  (cd "$repo_root/aisdk-service" && node build.mjs --sea)
  cp "$repo_root/aisdk-service/dist/mework-aisdk.exe" "$repo_root/aisdk-service/dist/mework-aisdk"
fi

# --- agent-client configuration ----------------------------------------------
# `.gitignore` keeps /AGENTS.md, /CLAUDE.md, /.claude/ and /.codex/ out of the
# repository — they are per-developer — but
# scripts/tests/debug-client-entrypoints.test.mjs asserts their exact shape, so
# `npm test` is red on a fresh clone until they exist. Writing an agent's hook
# configuration into someone's checkout is not a thing a setup script should do
# behind their back: by default this only reports what is missing, and
# `--seed-agent-config` writes the minimum the test requires.
step "agent-client configuration"
missing_agent_config=()
for relative in AGENTS.md CLAUDE.md .claude/launch.json .claude/settings.json .codex/hooks.json; do
  [ -e "$repo_root/$relative" ] || missing_agent_config+=("$relative")
done
if [ "${#missing_agent_config[@]}" -eq 0 ]; then
  echo "  present"
elif [ "$seed_agent_config" -eq 0 ]; then
  echo "  missing: ${missing_agent_config[*]}"
  echo "  npm run test:debug-client-entrypoints stays red until these exist;"
  echo "  re-run with --seed-agent-config to write them."
else
  mkdir -p "$repo_root/.claude" "$repo_root/.codex"
  for relative in "${missing_agent_config[@]}"; do
    case "$relative" in
      AGENTS.md)
        cat > "$repo_root/AGENTS.md" <<'SEED'
# Mework — project contract for coding agents

The native window (`npm run tauri:dev`) is invisible to an agent and holds the
terminal. Use the browser bridge, which serves the UI of the same Rust host on
http://127.0.0.1:1420:

    npm run dev:browser -- --codex     # from Codex
    npm run dev:browser -- --claude    # from Claude Code

`scripts/browser-dev-command-hook.mjs`, configured as a PreToolUse hook,
rewrites a bare `npm run tauri:dev` into the calling client's flag and denies it
when it carries arguments or is chained.

Before handing work back: `npm test`, and `cargo test` in `src-tauri/`.
SEED
        ;;
      CLAUDE.md) printf '@AGENTS.md\n' > "$repo_root/CLAUDE.md" ;;
      .claude/launch.json)
        cat > "$repo_root/.claude/launch.json" <<'SEED'
{
  "version": "0.2.0",
  "configurations": [
    {
      "name": "mework-dev-browser-claude-start",
      "runtimeExecutable": "npm",
      "runtimeArgs": ["run", "dev:browser", "--", "--claude"],
      "port": 1420,
      "autoPort": false,
      "url": "http://127.0.0.1:1420"
    },
    {
      "name": "mework-dev-browser-shared-attach",
      "url": "http://127.0.0.1:1420"
    }
  ]
}
SEED
        ;;
      .claude/settings.json)
        cat > "$repo_root/.claude/settings.json" <<'SEED'
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "command": "node",
            "args": [
              "${CLAUDE_PROJECT_DIR}/scripts/browser-dev-command-hook.mjs",
              "--claude"
            ]
          }
        ]
      }
    ]
  }
}
SEED
        ;;
      .codex/hooks.json)
        cat > "$repo_root/.codex/hooks.json" <<'SEED'
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "^(Bash|shell_command)$",
        "hooks": [
          {
            "command": "node \"$(git rev-parse --show-toplevel)/scripts/browser-dev-command-hook.mjs\" --codex",
            "commandWindows": "node \"$(git rev-parse --show-toplevel)/scripts/browser-dev-command-hook.mjs\" --codex"
          }
        ]
      }
    ]
  }
}
SEED
        ;;
    esac
    echo "  wrote $relative"
  done
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

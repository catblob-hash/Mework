#!/usr/bin/env bash
# Reports the untracked agent-client files a Mework checkout needs for
# `npm test`, and with `--write` creates the minimum the test asserts on.
#
# Shared by scripts/setup-linux-dev.sh and scripts/setup-macos-dev.sh, which
# pass `--write` for their own `--seed-agent-config`. Must stay bash 3.2
# compatible: that is the bash macOS ships.
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
seed_agent_config=0
for argument in "$@"; do
  case "$argument" in
    --write) seed_agent_config=1 ;;
    *) echo "unknown argument: $argument" >&2; exit 2 ;;
  esac
done

# --- agent-client configuration ----------------------------------------------
# `.gitignore` keeps /AGENTS.md, /CLAUDE.md, /.claude/ and /.codex/ out of the
# repository — they are per-developer — but
# scripts/tests/debug-client-entrypoints.test.mjs asserts their exact shape, so
# `npm test` is red on a fresh clone until they exist. Writing an agent's hook
# configuration into someone's checkout is not a thing a setup script should do
# behind their back: by default this only reports what is missing, and
# `--seed-agent-config` writes the minimum the test requires.
missing_agent_config=()
for relative in AGENTS.md CLAUDE.md .claude/launch.json .claude/settings.json .codex/hooks.json; do
  [ -e "$repo_root/$relative" ] || missing_agent_config+=("$relative")
done
if [ "${#missing_agent_config[@]}" -eq 0 ]; then
  echo "  present"
elif [ "$seed_agent_config" -eq 0 ]; then
  echo "  missing: ${missing_agent_config[*]}"
  echo "  npm run test:debug-client-entrypoints stays red until these exist;"
  echo "  re-run the setup script with --seed-agent-config to write them."
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

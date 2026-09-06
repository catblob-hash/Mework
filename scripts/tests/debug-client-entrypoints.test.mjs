import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  ".."
);
const hook = path.join(root, "scripts", "browser-dev-command-hook.mjs");

function readJson(relativePath) {
  return JSON.parse(readFileSync(path.join(root, relativePath), "utf8"));
}

function runHook(clientFlag, toolName, command) {
  const child = spawn(process.execPath, [hook, clientFlag], {
    cwd: root,
    stdio: ["pipe", "pipe", "pipe"]
  });
  child.stdin.end(JSON.stringify({
    hook_event_name: "PreToolUse",
    tool_name: toolName,
    tool_input: { command }
  }));
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => {
      resolve({ code, signal, stdout, stderr });
    });
  });
}

function runConfiguredCommand(executable, args, cwd, input) {
  const child = spawn(executable, args, {
    cwd,
    stdio: ["pipe", "pipe", "pipe"]
  });
  child.stdin.end(JSON.stringify(input));
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => {
      resolve({ code, signal, stdout, stderr });
    });
  });
}

// 两个必需的调试入口。其余本机启动器（临时端口、假 issuer、文档示例等）由别的
// 会话按需增删，不属于本检查的判据。
const requiredLaunchEntrypoints = [
  {
    name: "mework-dev-browser-claude-start",
    runtimeExecutable: "npm",
    runtimeArgs: ["run", "dev:browser", "--", "--claude"],
    port: 1420,
    autoPort: false,
    url: "http://127.0.0.1:1420"
  },
  {
    name: "mework-dev-browser-shared-attach",
    url: "http://127.0.0.1:1420"
  }
];

// 按名称定位两个必需入口，各自要求恰好出现一次且形状逐字段相等（多一个键也算不
// 相等，所以 attach 依然不许带启动命令）。额外名称的本机启动器一律放行。
function claudeLaunchEntrypointProblems(configurations) {
  if (!Array.isArray(configurations)) {
    return ["launch.json 的 configurations 不是数组"];
  }
  const problems = [];
  for (const expected of requiredLaunchEntrypoints) {
    const matches = configurations.filter(
      (configuration) =>
        configuration !== null &&
        typeof configuration === "object" &&
        configuration.name === expected.name
    );
    if (matches.length !== 1) {
      problems.push(
        `${expected.name} 必须恰好出现一次，实际出现 ${matches.length} 次`
      );
      continue;
    }
    try {
      assert.deepEqual(matches[0], expected);
    } catch (error) {
      problems.push(`${expected.name} 的配置与必需形状不一致：${error.message}`);
    }
  }
  return problems;
}

test("Claude launch separates exact-port startup from external-owner attach", () => {
  const launch = readJson(".claude/launch.json");
  assert.deepEqual(claudeLaunchEntrypointProblems(launch.configurations), []);
});

test("extra local launchers never break the required Claude entrypoints", () => {
  const documented = {
    name: "mework-dev-browser-docs",
    runtimeExecutable: "npm",
    runtimeArgs: ["run", "site:preview"],
    port: 4321
  };
  const temporary = {
    name: "mework-dev-browser-claude-1520",
    runtimeExecutable: "cmd",
    runtimeArgs: ["/c", "set MEWORK_BROWSER_DEV_FRONTEND_PORT=1520&& npm run dev:browser -- --claude"],
    port: 1520,
    autoPort: false,
    url: "http://127.0.0.1:1520"
  };
  assert.deepEqual(claudeLaunchEntrypointProblems(requiredLaunchEntrypoints), []);
  assert.deepEqual(
    claudeLaunchEntrypointProblems([
      requiredLaunchEntrypoints[0],
      documented,
      requiredLaunchEntrypoints[1],
      temporary
    ]),
    []
  );
  assert.deepEqual(
    claudeLaunchEntrypointProblems([
      temporary,
      requiredLaunchEntrypoints[1],
      documented,
      requiredLaunchEntrypoints[0]
    ]),
    []
  );
  // 旧判据（整个数组与二项数组完全相等）会拒绝这些本机启动器，这正是入口检查
  // 恒红、&& 链在 Vitest 之前中断的根因。
  assert.throws(() => {
    assert.deepEqual(
      [requiredLaunchEntrypoints[0], documented, requiredLaunchEntrypoints[1]],
      requiredLaunchEntrypoints
    );
  });
});

test("required Claude entrypoints stay strictly constrained", () => {
  const [start, attach] = requiredLaunchEntrypoints;
  const cases = [
    ["缺失 start", [attach]],
    ["缺失 attach", [start]],
    ["重复 start", [start, { ...start }, attach]],
    [
      "错误的客户端参数",
      [{ ...start, runtimeArgs: ["run", "dev:browser", "--", "--codex"] }, attach]
    ],
    ["错误端口", [{ ...start, port: 1520 }, attach]],
    ["允许自动换端口", [{ ...start, autoPort: true }, attach]],
    [
      "start 缺少 autoPort",
      [
        {
          name: start.name,
          runtimeExecutable: start.runtimeExecutable,
          runtimeArgs: start.runtimeArgs,
          port: start.port,
          url: start.url
        },
        attach
      ]
    ],
    ["错误的 attach 地址", [start, { ...attach, url: "http://localhost:1420" }]],
    [
      "attach 带启动命令",
      [start, { ...attach, runtimeExecutable: "npm", runtimeArgs: ["run", "dev:browser"] }]
    ],
    ["configurations 不是数组", { name: "mework-dev-browser-claude-start" }]
  ];
  for (const [label, configurations] of cases) {
    const problems = claudeLaunchEntrypointProblems(configurations);
    assert.equal(problems.length > 0, true, `${label} 应当被拒绝`);
  }
});

test("Codex and Claude hooks use the same bridge hook with distinct clients", () => {
  const codex = readJson(".codex/hooks.json");
  const claude = readJson(".claude/settings.json");
  const codexHook = codex.hooks.PreToolUse[0];
  const claudeHook = claude.hooks.PreToolUse[0];
  assert.equal(codexHook.matcher, "^(Bash|shell_command)$");
  assert.equal(
    codexHook.hooks[0].command,
    "node \"$(git rev-parse --show-toplevel)/scripts/browser-dev-command-hook.mjs\" --codex"
  );
  assert.equal(claudeHook.matcher, "Bash");
  assert.equal(claudeHook.hooks[0].command, "node");
  assert.deepEqual(claudeHook.hooks[0].args, [
    "${CLAUDE_PROJECT_DIR}/scripts/browser-dev-command-hook.mjs",
    "--claude"
  ]);
});

test("configured hooks resolve the project script from a nested session cwd", async () => {
  const codex = readJson(".codex/hooks.json").hooks.PreToolUse[0].hooks[0];
  const claude = readJson(".claude/settings.json").hooks.PreToolUse[0].hooks[0];
  const nestedCwd = path.join(root, "src");
  const input = {
    hook_event_name: "PreToolUse",
    tool_name: "Bash",
    tool_input: { command: "npm run tauri:dev" }
  };
  const codexInvocation = process.platform === "win32"
    ? [
        "powershell",
        ["-NoProfile", "-Command", codex.commandWindows]
      ]
    : ["sh", ["-c", codex.command]];
  const claudeArgs = claude.args.map((argument) =>
    argument.replace("${CLAUDE_PROJECT_DIR}", root)
  );
  const [codexResult, claudeResult] = await Promise.all([
    runConfiguredCommand(...codexInvocation, nestedCwd, input),
    runConfiguredCommand(claude.command, claudeArgs, nestedCwd, input)
  ]);
  assert.equal(codexResult.code, 0, codexResult.stderr);
  assert.equal(claudeResult.code, 0, claudeResult.stderr);
  assert.equal(
    JSON.parse(codexResult.stdout).hookSpecificOutput.updatedInput.command,
    "npm run dev:browser -- --codex"
  );
  assert.equal(
    JSON.parse(claudeResult.stdout).hookSpecificOutput.updatedInput.command,
    "npm run dev:browser -- --claude"
  );
});

test("shared hook routes each client through its exact browser-dev flag", async () => {
  const [codex, claude] = await Promise.all([
    runHook("--codex", "shell_command", "npm run tauri:dev"),
    runHook("--claude", "Bash", "npm run tauri:dev")
  ]);
  assert.equal(codex.code, 0, codex.stderr);
  assert.equal(claude.code, 0, claude.stderr);
  assert.equal(
    JSON.parse(codex.stdout).hookSpecificOutput.updatedInput.command,
    "npm run dev:browser -- --codex"
  );
  assert.equal(
    JSON.parse(claude.stdout).hookSpecificOutput.updatedInput.command,
    "npm run dev:browser -- --claude"
  );
  assert.equal(
    Object.hasOwn(
      JSON.parse(codex.stdout).hookSpecificOutput,
      "permissionDecision"
    ),
    true
  );
  assert.equal(
    JSON.parse(codex.stdout).hookSpecificOutput.permissionDecision,
    "allow"
  );
});

test("shared hook leaves tests/builds unchanged and rejects tauri:dev arguments", async () => {
  const [testCommand, nativeBuild, unsupportedDevArguments] = await Promise.all([
    runHook("--codex", "shell_command", "npm test"),
    runHook("--claude", "Bash", "npm run tauri:build"),
    runHook("--claude", "Bash", "npm run tauri:dev -- --features unsafe")
  ]);
  assert.equal(testCommand.code, 0, testCommand.stderr);
  assert.equal(testCommand.stdout, "");
  assert.equal(nativeBuild.code, 0, nativeBuild.stderr);
  assert.equal(nativeBuild.stdout, "");
  assert.equal(unsupportedDevArguments.code, 0, unsupportedDevArguments.stderr);
  assert.equal(
    JSON.parse(unsupportedDevArguments.stdout).hookSpecificOutput.permissionDecision,
    "deny"
  );
});

test("tauri:dev command chains are denied instead of approving the whole chain", async () => {
  const chained = await runHook(
    "--codex",
    "shell_command",
    "npm run tauri:dev && npm test"
  );
  assert.equal(chained.code, 0, chained.stderr);
  const output = JSON.parse(chained.stdout).hookSpecificOutput;
  assert.equal(output.permissionDecision, "deny");
  assert.match(output.permissionDecisionReason, /带参数或链式/);
  assert.equal(Object.hasOwn(output, "updatedInput"), false);
});

test("Claude instructions inherit the canonical project contract", () => {
  const agents = readFileSync(path.join(root, "AGENTS.md"), "utf8");
  const claude = readFileSync(path.join(root, "CLAUDE.md"), "utf8");
  assert.match(agents, /npm run dev:browser -- --codex/);
  assert.match(agents, /npm run dev:browser -- --claude/);
  assert.match(claude, /^@AGENTS\.md/m);
});

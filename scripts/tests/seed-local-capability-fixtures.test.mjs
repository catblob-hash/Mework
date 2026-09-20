// Tests for the pure planning/merging half of
// `scripts/seed-local-capability-fixtures.mjs`, plus one end-to-end pass over a
// temporary home and workspace. Nothing here reads or writes the real user
// profile: the fixtures are planned against fake paths, and the file work runs
// in `mkdtempSync` directories that are removed afterwards.

import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  GLOBAL_HOOKS,
  GLOBAL_SKILLS,
  WORKSPACE_HOOK_EVENT,
  WORKSPACE_SKILLS,
  documentIsEmpty,
  fixtureMcpServers,
  fixtureWorkspaceMcpServers,
  mergeFixtureHooks,
  mergeMcpConfig,
  parseFixtureArguments,
  planFixturePaths,
  removeFixtureMcpServers,
  removeFixtures,
  renderSkillManifest,
  seedFixtures,
  skillDirectoryNameIsValid,
  workspaceHookHandlers
} from "../seed-local-capability-fixtures.mjs";

/** Keys `mcp.json` defines for a stdio server, plus the Mework extensions. */
const STDIO_KEYS = new Set([
  "command",
  "args",
  "env",
  "description",
  "timeoutSeconds",
  "longRunning",
  "disabledTools"
]);
/** Keys `mcp.json` defines for an http server, plus the Mework extensions. */
const HTTP_KEYS = new Set([
  "type",
  "url",
  "headers",
  "description",
  "timeoutSeconds",
  "longRunning",
  "disabledTools"
]);

// ---- mcp.json ---------------------------------------------------------------

test("merges fixture servers into an existing mcp.json without touching the user's keys", () => {
  const existing = {
    note: "mine",
    mcpServers: { my_server: { command: "my-tool", args: ["--x"] } }
  };

  const merged = mergeMcpConfig(existing, fixtureMcpServers("C:/work"));

  assert.equal(merged.note, "mine", "顶层未知键要原样保留");
  assert.deepEqual(merged.mcpServers.my_server, { command: "my-tool", args: ["--x"] });
  assert.deepEqual(Object.keys(merged.mcpServers), [
    "my_server",
    "fixture_filesystem",
    "fixture_git",
    "fixture_everything_http"
  ]);
  assert.equal(merged.mcpServers.fixture_filesystem.command, "npx");
  assert.equal(merged.mcpServers.fixture_git.command, "uvx");
  assert.equal(merged.mcpServers.fixture_everything_http.type, "http");
  // The input is not mutated: a plan never rewrites the document it read.
  assert.deepEqual(existing.mcpServers, { my_server: { command: "my-tool", args: ["--x"] } });
});

test("a re-run replaces the fixture keys instead of stacking a second copy", () => {
  const existing = {
    mcpServers: {
      my_server: { command: "my-tool" },
      fixture_filesystem: { command: "stale" }
    }
  };

  const once = mergeMcpConfig(existing, fixtureMcpServers("C:/work"));
  const twice = mergeMcpConfig(once, fixtureMcpServers("C:/work"));

  assert.deepEqual(twice, once);
  assert.equal(twice.mcpServers.fixture_filesystem.command, "npx", "重跑必须换成本次的夹具");
  assert.deepEqual(Object.keys(twice.mcpServers), [
    "my_server",
    "fixture_filesystem",
    "fixture_git",
    "fixture_everything_http"
  ]);
});

test("--remove drops every fixture_ server and leaves the user's key", () => {
  const merged = mergeMcpConfig(
    { mcpServers: { my_server: { command: "my-tool" } } },
    fixtureMcpServers("C:/work")
  );

  const removed = removeFixtureMcpServers(merged);

  assert.deepEqual(removed.mcpServers, { my_server: { command: "my-tool" } });
  // A document that only ever held fixtures comes back as the empty shell the
  // caller deletes instead of writing.
  assert.deepEqual(removeFixtureMcpServers({ mcpServers: fixtureMcpServers("C:/work") }), {
    mcpServers: {}
  });
});

test("fixture servers use the mcp.json shape for their transport and nothing retired", () => {
  const servers = { ...fixtureMcpServers("C:/work"), ...fixtureWorkspaceMcpServers() };

  for (const [name, entry] of Object.entries(servers)) {
    assert.ok(name.startsWith("fixture_"), `${name} 必须由夹具拥有`);
    assert.equal(typeof entry.description, "string", `${name} 需要目录里显示的描述`);
    const allowed = entry.type === "http" ? HTTP_KEYS : STDIO_KEYS;
    for (const key of Object.keys(entry)) {
      assert.ok(
        allowed.has(key),
        `${name} 里的 ${key} 不是 mcp.json 的键，文档里的注册表字段不该再出现`
      );
    }
  }

  const filesystem = servers.fixture_filesystem;
  assert.equal(filesystem.command, "npx");
  assert.ok(filesystem.args.includes("C:/work"), "挂载目录要出现在参数里");
  assert.deepEqual(filesystem.env, {});
  assert.equal(filesystem.url, undefined, "stdio 条目不该有 url");

  const http = servers.fixture_everything_http;
  assert.equal(http.type, "http");
  assert.equal(http.url, "http://127.0.0.1:3001/mcp");
  assert.deepEqual(http.headers, {});
  assert.equal(http.command, undefined, "http 条目不该有 command");

  assert.deepEqual(Object.keys(fixtureMcpServers("x")).sort(), [
    "fixture_everything_http",
    "fixture_filesystem",
    "fixture_git"
  ]);
  assert.deepEqual(Object.keys(fixtureWorkspaceMcpServers()), ["fixture_workspace_echo"]);
});

// ---- hooks.json -------------------------------------------------------------

const USER_HOOKS = {
  hooks: {
    Stop: [
      {
        hooks: [{ type: "command", name: "用户的钩子", command: "echo mine", timeout: 30 }]
      }
    ],
    PostToolUse: [
      {
        matcher: "^(MCP__)",
        hooks: [{ type: "command", name: "用户自己的后置钩子", command: "echo theirs", timeout: 30 }]
      }
    ]
  }
};

test("merges the fixture hook into an existing hooks.json without touching user hooks", () => {
  const merged = mergeFixtureHooks(USER_HOOKS, WORKSPACE_HOOK_EVENT, workspaceHookHandlers());

  assert.deepEqual(merged.hooks.Stop, USER_HOOKS.hooks.Stop, "别的 event 原样保留");
  assert.deepEqual(merged.hooks[WORKSPACE_HOOK_EVENT][0], USER_HOOKS.hooks[WORKSPACE_HOOK_EVENT][0]);
  const appended = merged.hooks[WORKSPACE_HOOK_EVENT][1];
  assert.deepEqual(appended.hooks, workspaceHookHandlers());
  assert.ok(appended.matcher.includes("bash"), "夹具组自带 matcher");
  // The input document is not mutated.
  assert.equal(USER_HOOKS.hooks[WORKSPACE_HOOK_EVENT].length, 1);
});

test("a re-run replaces the fixture hook rather than adding a second one", () => {
  const once = mergeFixtureHooks(USER_HOOKS, WORKSPACE_HOOK_EVENT, workspaceHookHandlers());
  const twice = mergeFixtureHooks(once, WORKSPACE_HOOK_EVENT, workspaceHookHandlers());

  assert.deepEqual(twice, once);
  const handlers = twice.hooks[WORKSPACE_HOOK_EVENT].flatMap((group) => group.hooks);
  assert.equal(handlers.filter((handler) => handler.name.startsWith("fixture")).length, 1);
  assert.equal(handlers.filter((handler) => handler.name === "用户自己的后置钩子").length, 1);
});

test("--remove takes the fixture hook out and leaves every user hook", () => {
  const merged = mergeFixtureHooks(USER_HOOKS, WORKSPACE_HOOK_EVENT, workspaceHookHandlers());

  const removed = mergeFixtureHooks(merged, WORKSPACE_HOOK_EVENT, []);

  assert.deepEqual(removed, USER_HOOKS);
  // A file the fixture created on its own collapses to the empty shell.
  const seeded = mergeFixtureHooks(undefined, WORKSPACE_HOOK_EVENT, workspaceHookHandlers());
  assert.deepEqual(mergeFixtureHooks(seeded, WORKSPACE_HOOK_EVENT, []), { hooks: {} });
});

test("recognises the empty shell a fully fixture-owned file leaves behind", () => {
  assert.equal(documentIsEmpty({}, "mcpServers"), true);
  assert.equal(documentIsEmpty({ mcpServers: {} }, "mcpServers"), true);
  assert.equal(documentIsEmpty({ mcpServers: { a: {} } }, "mcpServers"), false);
  assert.equal(documentIsEmpty({ note: "mine", mcpServers: {} }, "mcpServers"), false);
  assert.equal(documentIsEmpty({ hooks: {} }, "hooks"), true);
  assert.equal(documentIsEmpty({ hooks: { Stop: [] } }, "hooks"), false);
});

// ---- paths and arguments ----------------------------------------------------

test("lists the global paths and, only with a workspace, the project ones", () => {
  const home = join("C:", "Users", "tester");
  const workspace = join("C:", "project", "repo");

  const globalOnly = planFixturePaths(home, null);
  assert.equal(globalOnly.workspace, null);
  assert.equal(globalOnly.global.config, join(home, ".mework"));
  assert.equal(globalOnly.global.hooks, join(home, ".mework", "hooks.json"));
  assert.equal(globalOnly.global.hooksBackup, join(home, ".mework", "hooks.json.before-fixtures"));
  assert.equal(globalOnly.global.mcp, join(home, ".mework", "mcp.json"));
  assert.equal(globalOnly.global.mcpBackup, join(home, ".mework", "mcp.json.before-fixtures"));
  assert.equal(globalOnly.global.skills, join(home, ".mework", "skills"));
  assert.deepEqual(globalOnly.global.skillDirectories, [
    join(home, ".mework", "skills", "fixture-code-review"),
    join(home, ".mework", "skills", "fixture-release-notes"),
    join(home, ".mework", "skills", "fixture-incident-triage")
  ]);

  const both = planFixturePaths(home, workspace);
  assert.equal(both.global.mcp, globalOnly.global.mcp, "项目级不能改变全局路径");
  assert.equal(both.workspace.config, join(workspace, ".mework"));
  assert.equal(both.workspace.hooks, join(workspace, ".mework", "hooks.json"));
  assert.equal(both.workspace.mcp, join(workspace, ".mework", "mcp.json"));
  assert.deepEqual(both.workspace.skillDirectories, [
    join(workspace, ".mework", "skills", "fixture-workspace-notes")
  ]);
});

test("parses --workspace, --remove and --help and rejects what it does not know", () => {
  assert.deepEqual(parseFixtureArguments([]), { help: false, remove: false, workspace: null });
  assert.deepEqual(parseFixtureArguments(["--remove"]), {
    help: false,
    remove: true,
    workspace: null
  });
  assert.deepEqual(parseFixtureArguments(["--workspace", "C:/repo", "--remove"]), {
    help: false,
    remove: true,
    workspace: "C:/repo"
  });
  assert.equal(parseFixtureArguments(["--help"]).help, true);

  assert.throws(() => parseFixtureArguments(["--nope"]), /不支持的参数/u);
  assert.throws(() => parseFixtureArguments(["--workspace"]), /需要一个工作区路径/u);
  assert.throws(() => parseFixtureArguments(["--workspace", "--remove"]), /需要一个工作区路径/u);
  assert.throws(() => parseFixtureArguments(["--remove", "--remove"]), /不能重复/u);
  assert.throws(
    () => parseFixtureArguments(["--workspace", "a", "--workspace", "b"]),
    /不能重复/u
  );
});

// ---- skill manifests --------------------------------------------------------

test("every skill manifest carries the flat frontmatter the host reads", () => {
  const skills = [...GLOBAL_SKILLS, ...WORKSPACE_SKILLS];
  assert.equal(
    new Set(skills.map((skill) => skill.directory)).size,
    skills.length,
    "目录名就是模型看到的名字，不能重复"
  );

  for (const skill of skills) {
    assert.ok(skill.directory.startsWith("fixture-"), `${skill.directory} 必须以 fixture- 开头`);
    assert.ok(
      skillDirectoryNameIsValid(skill.directory),
      `${skill.directory} 是发现会直接跳过的目录名`
    );

    const manifest = renderSkillManifest(skill);
    const lines = manifest.split("\n");
    assert.equal(lines[0], "---");
    const end = lines.indexOf("---", 1);
    assert.ok(end > 0, "frontmatter 必须有收尾的 ---");
    const frontmatter = lines.slice(1, end);
    for (const key of ["name", "description", "when_to_use", "version", "author", "tags"]) {
      assert.ok(
        frontmatter.some((line) => line.startsWith(`${key}:`)),
        `${skill.directory} 的 frontmatter 缺 ${key}`
      );
    }
    assert.equal(lines[end + 1], "", "frontmatter 与正文之间空一行");
    assert.ok(lines[end + 2].startsWith("# "), "正文以一级标题开头");
    assert.ok(manifest.endsWith(`${skill.body}\n`));
  }

  // The rule the directory names are checked against, spelled out.
  assert.equal(skillDirectoryNameIsValid("fixture-code-review"), true);
  assert.equal(skillDirectoryNameIsValid(""), false);
  assert.equal(skillDirectoryNameIsValid(" fixture-x"), false);
  assert.equal(skillDirectoryNameIsValid("fixture(x)"), false);
  assert.equal(skillDirectoryNameIsValid("fixture,x"), false);
  assert.equal(skillDirectoryNameIsValid("fixture\tx"), false);
});

// ---- end to end -------------------------------------------------------------

test("seeds and removes a temporary home and workspace without losing user content", () => {
  const root = mkdtempSync(join(tmpdir(), "mework-capability-fixtures-"));
  const home = join(root, "home");
  const workspace = join(root, "workspace");
  const userHooks = {
    hooks: { Stop: [{ hooks: [{ type: "command", name: "用户的钩子", command: "echo mine" }] }] }
  };
  const userMcp = { note: "mine", mcpServers: { my_server: { command: "my-tool" } } };
  const workspaceMcp = { mcpServers: { workspace_user_server: { command: "ws-tool" } } };

  try {
    mkdirSync(join(home, ".mework"), { recursive: true });
    mkdirSync(join(workspace, ".mework"), { recursive: true });
    writeFileSync(join(home, ".mework", "hooks.json"), `${JSON.stringify(userHooks, null, 2)}\n`);
    writeFileSync(join(home, ".mework", "mcp.json"), `${JSON.stringify(userMcp, null, 2)}\n`);
    writeFileSync(
      join(workspace, ".mework", "mcp.json"),
      `${JSON.stringify(workspaceMcp, null, 2)}\n`
    );

    const paths = planFixturePaths(home, workspace);
    seedFixtures(paths, workspace);
    seedFixtures(paths, workspace);

    for (const directory of [...paths.global.skillDirectories, ...paths.workspace.skillDirectories]) {
      assert.ok(existsSync(join(directory, "SKILL.md")), `${directory} 里应有 SKILL.md`);
    }

    // The backup keeps the first original, not the fixture a re-run wrote.
    assert.deepEqual(
      JSON.parse(readFileSync(paths.global.hooksBackup, "utf8")),
      userHooks,
      "hooks.json.before-fixtures 必须是替换前的原文"
    );
    assert.deepEqual(JSON.parse(readFileSync(paths.global.hooks, "utf8")), GLOBAL_HOOKS);
    assert.deepEqual(
      JSON.parse(readFileSync(paths.global.mcpBackup, "utf8")),
      userMcp,
      "mcp.json.before-fixtures 必须是首次运行前的原文"
    );
    const seededMcp = JSON.parse(readFileSync(paths.global.mcp, "utf8"));
    assert.equal(seededMcp.note, "mine");
    assert.deepEqual(Object.keys(seededMcp.mcpServers).sort(), [
      "fixture_everything_http",
      "fixture_filesystem",
      "fixture_git",
      "my_server"
    ]);
    assert.deepEqual(
      JSON.parse(readFileSync(paths.workspace.mcp, "utf8")).mcpServers.workspace_user_server,
      { command: "ws-tool" }
    );

    // One fixture handler after two runs, not two.
    const seededWorkspaceHooks = JSON.parse(readFileSync(paths.workspace.hooks, "utf8"));
    const handlers = seededWorkspaceHooks.hooks[WORKSPACE_HOOK_EVENT].flatMap(
      (group) => group.hooks
    );
    assert.equal(handlers.filter((handler) => handler.name.startsWith("fixture")).length, 1);

    removeFixtures(paths);

    // The user's hooks.json came back out of its backup, which is now gone.
    assert.equal(existsSync(paths.global.hooksBackup), false);
    assert.deepEqual(JSON.parse(readFileSync(paths.global.hooks, "utf8")), userHooks);
    assert.equal(existsSync(paths.global.mcpBackup), false);
    assert.deepEqual(JSON.parse(readFileSync(paths.global.mcp, "utf8")), userMcp);
    assert.deepEqual(JSON.parse(readFileSync(paths.workspace.mcp, "utf8")), workspaceMcp);
    // These two files only ever held fixtures, so they are gone rather than
    // left as `{"mcpServers":{}}` shells.
    assert.equal(existsSync(paths.workspace.hooks), false);
    for (const directory of [...paths.global.skillDirectories, ...paths.workspace.skillDirectories]) {
      assert.equal(existsSync(directory), false, `${directory} 应已删除`);
    }
    assert.equal(existsSync(paths.global.skills), false, "空的 skills/ 也要收掉");
    assert.equal(existsSync(paths.workspace.skills), false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("--remove leaves a hooks.json the user edited after seeding, and its backup", () => {
  const root = mkdtempSync(join(tmpdir(), "mework-capability-fixtures-edited-"));
  const home = join(root, "home");
  const original = {
    hooks: { Stop: [{ hooks: [{ type: "command", name: "用户的钩子", command: "echo mine" }] }] }
  };

  try {
    mkdirSync(join(home, ".mework"), { recursive: true });
    writeFileSync(join(home, ".mework", "hooks.json"), `${JSON.stringify(original, null, 2)}\n`);
    const paths = planFixturePaths(home, null);
    seedFixtures(paths, root);

    // The user edits the seeded file, so the fixtures are theirs to keep and
    // the backup is the only copy of what they had before.
    const edited = JSON.parse(readFileSync(paths.global.hooks, "utf8"));
    edited.hooks.Stop = [
      { hooks: [{ type: "command", name: "用户新增的钩子", command: "echo added" }] }
    ];
    writeFileSync(paths.global.hooks, `${JSON.stringify(edited, null, 2)}\n`);

    removeFixtures(paths);

    assert.deepEqual(JSON.parse(readFileSync(paths.global.hooks, "utf8")), edited);
    assert.deepEqual(JSON.parse(readFileSync(paths.global.hooksBackup, "utf8")), original);
    // The fixture-only mcp.json still goes, fixture keys and all.
    assert.equal(existsSync(paths.global.mcp), false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

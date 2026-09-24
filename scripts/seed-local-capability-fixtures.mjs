#!/usr/bin/env node
// Seeds this machine with a few real skills, MCP servers and hooks, so the
// conversation-settings catalog pages have something to draw.
//
// All three kinds are file discovery now, and both the packaged app and a
// browser-dev session read the same `~/.mework` files (plus each workspace's
// own `.mework`), so nothing here touches an app-data directory or the
// persisted document. The app owns its document while it is running, and
// editing that behind its back would be overwritten on its next save; the files
// below are the app's input, not its state.
//
//   ~/.mework/hooks.json                 the global hook file
//   ~/.mework/skills/<dir>/SKILL.md      one directory per skill; the directory
//                                        name is the name the model addresses
//   ~/.mework/mcp.json                   { "mcpServers": { … } }, Claude Code's
//                                        .mcp.json shape
//   <workspace>/.mework/…                the same three at the project level
//
// Write behaviour:
//   * the global hooks.json is replaced wholesale, so its original is kept once
//     as `hooks.json.before-fixtures`;
//   * the `fixture_*` servers are merged into `mcp.json`, whose original is kept
//     once as `mcp.json.before-fixtures`;
//   * the project level is merged the same way but without a backup.
// Re-running replaces exactly the entries this script owns — keys starting with
// `fixture_`, directories starting with `fixture-`, hook handlers whose `name`
// starts with `fixture` — and leaves every other entry alone. `--remove` takes
// the fixtures back out of both levels. See `--help`.
//
// The pure planning and merging is exported below so it can be tested without a
// user profile; `main()` only reads arguments, calls these functions, and
// prints what happened.

import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const CONFIG_DIRECTORY = ".mework";
const SKILLS_DIRECTORY = "skills";
const SKILL_MANIFEST = "SKILL.md";
const HOOKS_FILE = "hooks.json";
const MCP_FILE = "mcp.json";
/** Suffix of the one backup per replaced file, written only on the first run. */
const BACKUP_SUFFIX = ".before-fixtures";
/** Marks an `mcpServers` key this script owns, so a re-run replaces exactly it. */
const FIXTURE_SERVER_PREFIX = "fixture_";
/** Marks a skill directory this script owns. */
const FIXTURE_SKILL_PREFIX = "fixture-";
/** Any hook handler whose `name` starts with this is replaced on a re-run. */
const FIXTURE_HOOK_NAME_PREFIX = "fixture";

// ---- Hooks: <level>/.mework/hooks.json --------------------------------------

export const WORKSPACE_HOOK_EVENT = "PostToolUse";
/** Both the global and the project hook only care about the local shell tools. */
const SHELL_HOOK_MATCHER = "^(powershell|bash)$";

// On Windows the host runs a hook command through PowerShell, so these are
// PowerShell one-liners. They only print, because a hook that changed anything
// would be a surprise the moment someone selects it.
export const GLOBAL_HOOKS = {
  hooks: {
    SessionStart: [
      {
        hooks: [
          {
            type: "command",
            name: "会话开始打点",
            command: "Write-Output 'mework hook: session started'",
            timeout: 30
          }
        ]
      }
    ],
    UserPromptSubmit: [
      {
        hooks: [
          {
            type: "command",
            name: "提交提示词打点",
            command: "Write-Output 'mework hook: prompt submitted'",
            timeout: 30
          }
        ]
      }
    ],
    PreToolUse: [
      {
        matcher: "^(read|write|edit)$",
        hooks: [
          {
            type: "command",
            name: "文件工具调用前打点",
            command: "Write-Output 'mework hook: file tool about to run'",
            timeout: 30
          }
        ]
      }
    ],
    PostToolUse: [
      {
        matcher: SHELL_HOOK_MATCHER,
        hooks: [
          {
            type: "command",
            name: "命令执行后打点",
            command: "Write-Output 'mework hook: shell tool finished'",
            timeout: 30
          }
        ]
      }
    ]
  }
};

/** The one project-level hook, merged into whatever `hooks.json` is there. */
export function workspaceHookHandlers() {
  return [
    {
      type: "command",
      name: "fixture 工作区工具调用后打点",
      command: "Write-Output 'mework fixture: workspace tool finished'",
      timeout: 30
    }
  ];
}

// ---- Skills: <level>/.mework/skills/<directory>/SKILL.md ---------------------

export const GLOBAL_SKILLS = [
  {
    directory: "fixture-code-review",
    name: "代码评审",
    description: "按缺陷类别逐项复查一段改动，给出可执行的修改建议。",
    whenToUse: "用户请求评审代码、检查 diff 或寻找缺陷时",
    tags: ["review", "quality"],
    body: [
      "# 代码评审",
      "",
      "按下面的顺序逐项过一遍，每一项都给出文件与行号：",
      "",
      "1. 正确性：边界条件、空值、并发与错误路径。",
      "2. 契约：调用方与被调用方对参数、返回值和失败模式的假设是否一致。",
      "3. 可读性：命名是否说明意图，注释是否只写了代码本身已经说清楚的事。",
      "4. 测试：新分支有没有对应的断言，断言是否会因为修复被撤销而变红。",
      "",
      "只报告能给出复现路径的问题；读不出复现路径的观感不要写进结论。"
    ].join("\n")
  },
  {
    directory: "fixture-release-notes",
    name: "发布说明",
    description: "把一串提交整理成面向用户的发布说明，按影响分组。",
    whenToUse: "用户要写 release notes、更新日志或版本说明时",
    tags: ["release", "writing"],
    body: [
      "# 发布说明",
      "",
      "先读提交记录，再按「用户能看见什么」分组，而不是按目录分组：",
      "",
      "- 新增：用户现在能做而之前做不到的事。",
      "- 修复：用户此前会撞上的问题，写清什么条件下会发生。",
      "- 变更：行为变了但没坏，尤其是需要用户改动配置的部分。",
      "",
      "纯内部重构不单独成条，除非它改变了性能或资源占用的数量级。"
    ].join("\n")
  },
  {
    directory: "fixture-incident-triage",
    name: "故障分诊",
    description: "从一份报错或日志出发，缩小范围到最小可复现，再定位成因。",
    whenToUse: "用户贴了报错、栈回溯或线上异常，需要定位原因时",
    tags: ["debug", "ops"],
    body: [
      "# 故障分诊",
      "",
      "1. 先固定事实：什么时间、哪个版本、哪条请求、报了什么。",
      "2. 再缩小范围：二分输入、二分版本、二分依赖，直到剩下最小可复现。",
      "3. 最后才谈成因，并且成因必须能解释「为什么之前是好的」。",
      "",
      "先改代码再解释现象的顺序是反的：没有复现就没有结论。"
    ].join("\n")
  }
];

/**
 * The project-level skill. It exists to make workspace scoping visible: a
 * conversation in another workspace, or a preset editor, is the only place it
 * should be reachable from.
 */
export const WORKSPACE_SKILLS = [
  {
    directory: "fixture-workspace-notes",
    name: "工作区笔记",
    description: "只在这个工作区里出现的技能，用来验证项目级 .mework/skills 的发现与作用域。",
    whenToUse: "用户要求记录或回顾当前工作区的事项时",
    tags: ["notes", "workspace"],
    body: [
      "# 工作区笔记",
      "",
      "这个技能来自 `<workspace>/.mework/skills/fixture-workspace-notes`：",
      "只有工作区挂在对话上时才应该出现在目录里。",
      "",
      "- 记下事实：谁在什么条件下会遇到这个问题。",
      "- 记下决定：为什么选它，以及被放弃的方案。",
      "- 记下下次的入口：从哪里继续。",
      "",
      "看不到这个技能，说明作用域过滤生效了；看得到而工作区不是当前这个，才是缺陷。"
    ].join("\n")
  }
];

/**
 * A `SKILL.md` with the flat frontmatter the host reads. It is deliberately not
 * YAML: the host accepts `key: value` lines only, and `tags` as `[a, b]`.
 */
export function renderSkillManifest(skill) {
  return [
    "---",
    `name: ${skill.name}`,
    `description: ${skill.description}`,
    `when_to_use: ${skill.whenToUse}`,
    "version: 1.0.0",
    "author: mework fixtures",
    `tags: [${skill.tags.join(", ")}]`,
    "---",
    "",
    skill.body,
    ""
  ].join("\n");
}

/**
 * The directory-name rule the host applies (`skills::directory_name_is_valid`),
 * mirrored so a fixture can never be written under a name discovery skips:
 * no surrounding whitespace, no control characters, and none of `(`, `)` or `,`.
 */
export function skillDirectoryNameIsValid(name) {
  return (
    name.length > 0 &&
    name.trim() === name &&
    ![...name].some((character) => {
      const code = character.codePointAt(0);
      return code < 0x20 || (code >= 0x7f && code <= 0x9f) || "(),".includes(character);
    })
  );
}

// ---- MCP servers: <level>/.mework/mcp.json -----------------------------------

/**
 * The three global fixture servers, in the `mcpServers` shape Claude Code's
 * `.mcp.json` uses. The two stdio servers mount `directory` — the repository
 * the script runs from — so the reference implementations have something to
 * read.
 *
 * Only keys the file format defines appear here. `description` is a Mework
 * extension; the other allowed ones (`timeoutSeconds`, `longRunning`,
 * `disabledTools`) are left out rather than written as defaults, and none of
 * the retired in-app registry fields (`enabled`, `provider`, `tags`, `id`,
 * timestamps) have a file form at all.
 */
export function fixtureMcpServers(directory) {
  return {
    fixture_filesystem: {
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-filesystem", directory],
      env: {},
      description: "把一个目录挂给模型读写，官方参考实现。"
    },
    fixture_git: {
      command: "uvx",
      args: ["mcp-server-git", "--repository", directory],
      env: {},
      description: "读取仓库状态、历史与 diff 的官方参考实现。"
    },
    fixture_everything_http: {
      type: "http",
      url: "http://127.0.0.1:3001/mcp",
      headers: {},
      description: "官方 everything 演示服务器，走 Streamable HTTP，用来验证非 stdio 传输。"
    }
  };
}

/** The one project-level server, a stdio entry like the reference ones. */
export function fixtureWorkspaceMcpServers() {
  return {
    fixture_workspace_echo: {
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-everything"],
      env: {},
      description: "工作区级的 echo 演示服务器，只在这个工作区的 mcp.json 里出现。"
    }
  };
}

/**
 * Merges servers into an `mcp.json` document. Every key the user wrote survives
 * exactly as it was — including top-level keys Mework does not read — and only
 * keys starting with `fixture_` are replaced, so a re-run is idempotent.
 */
export function mergeMcpConfig(existing, servers) {
  const document = isPlainObject(existing) ? { ...existing } : {};
  const current = isPlainObject(document.mcpServers) ? document.mcpServers : {};
  const merged = {};
  for (const [name, entry] of Object.entries(current)) {
    if (name.startsWith(FIXTURE_SERVER_PREFIX)) continue;
    merged[name] = entry;
  }
  Object.assign(merged, servers);
  document.mcpServers = merged;
  return document;
}

/** Drops every `fixture_` server, leaving the rest of the document untouched. */
export function removeFixtureMcpServers(existing) {
  return mergeMcpConfig(existing, {});
}

// ---- Merging ----------------------------------------------------------------

/**
 * Merges fixture handlers into a `hooks.json` document's `event`.
 *
 * Only the handlers this script owns — `name` starting with `fixture` — are
 * replaced, so a re-run does not stack a second copy; every other group,
 * handler, event and top-level key survives as written. A group left empty is
 * dropped, and so is the event key itself, which is what makes `--remove` leave
 * a file the script created reading as empty.
 */
export function mergeFixtureHooks(existing, event, handlers) {
  const document = isPlainObject(existing) ? { ...existing } : {};
  const current = isPlainObject(document.hooks) ? document.hooks : {};
  const groups = Array.isArray(current[event]) ? current[event] : [];
  const kept = groups
    .map((group) => {
      if (!isPlainObject(group)) return group;
      const groupHandlers = Array.isArray(group.hooks) ? group.hooks : [];
      return {
        ...group,
        hooks: groupHandlers.filter((handler) => !isFixtureHookHandler(handler))
      };
    })
    .filter(
      (group) =>
        !isPlainObject(group) || !Array.isArray(group.hooks) || group.hooks.length > 0
    );
  const next = [...kept];
  if (handlers.length > 0) next.push({ matcher: SHELL_HOOK_MATCHER, hooks: handlers });

  const hooks = { ...current };
  if (next.length === 0) delete hooks[event];
  else hooks[event] = next;
  document.hooks = hooks;
  return document;
}

function isFixtureHookHandler(handler) {
  return (
    isPlainObject(handler) &&
    typeof handler.name === "string" &&
    handler.name.trim().startsWith(FIXTURE_HOOK_NAME_PREFIX)
  );
}

/**
 * True when a document holds nothing but an empty container under `key` — the
 * shell this script leaves behind when its fixture entries were the only
 * content, so `--remove` deletes the file instead of writing that shell.
 */
export function documentIsEmpty(document, key) {
  if (!isPlainObject(document)) return true;
  if (Object.keys(document).some((name) => name !== key)) return false;
  const container = document[key];
  if (container === undefined) return true;
  return isPlainObject(container) && Object.keys(container).length === 0;
}

function isPlainObject(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

// ---- Path planning ----------------------------------------------------------

/**
 * Every path this script owns, derived from the two levels it writes: the
 * user's home directory (global) and an optional workspace path (project).
 * Nothing here touches the filesystem, so a test can plan against a fake home.
 */
export function planFixturePaths(home, workspace) {
  return {
    global: {
      ...levelPaths(home),
      skillDirectories: GLOBAL_SKILLS.map((skill) =>
        join(home, CONFIG_DIRECTORY, SKILLS_DIRECTORY, skill.directory)
      ),
      hooksBackup: join(home, CONFIG_DIRECTORY, `${HOOKS_FILE}${BACKUP_SUFFIX}`),
      mcpBackup: join(home, CONFIG_DIRECTORY, `${MCP_FILE}${BACKUP_SUFFIX}`)
    },
    workspace:
      workspace === null || workspace === undefined
        ? null
        : {
            ...levelPaths(workspace),
            skillDirectories: WORKSPACE_SKILLS.map((skill) =>
              join(workspace, CONFIG_DIRECTORY, SKILLS_DIRECTORY, skill.directory)
            )
          }
  };
}

function levelPaths(base) {
  const config = join(base, CONFIG_DIRECTORY);
  return {
    base,
    config,
    skills: join(config, SKILLS_DIRECTORY),
    hooks: join(config, HOOKS_FILE),
    mcp: join(config, MCP_FILE)
  };
}

// ---- Argument parsing -------------------------------------------------------

const USAGE = [
  "用法：node scripts/seed-local-capability-fixtures.mjs [--workspace <path>] [--remove] [--help]",
  "",
  "  （无参数）           写入全局夹具：~/.mework/{hooks.json, skills/fixture-*, mcp.json}",
  "  --workspace <path>   同时写入项目级夹具：<path>/.mework/{hooks.json, skills/fixture-*, mcp.json}",
  "  --remove             移除夹具；带上同一个 --workspace 才会连项目级一起移除",
  "  --help               显示这段说明",
  "",
  "写入方式：",
  "  ~/.mework/hooks.json   整体替换，原文件留在 hooks.json.before-fixtures",
  "  ~/.mework/mcp.json     合并进 fixture_* 键，原文件留在 mcp.json.before-fixtures",
  "  <path>/.mework/…       同样三项，项目级合并但不备份",
  "",
  "重新运行只替换本脚本自己的条目（fixture_* 键、fixture-* 目录、name 以 fixture 开头的钩子），",
  "其余内容原样保留。"
].join("\n");

export function parseFixtureArguments(args) {
  let workspace = null;
  let remove = false;
  let help = false;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--help" || argument === "-h") {
      help = true;
    } else if (argument === "--remove") {
      if (remove) throw new Error("--remove 不能重复");
      remove = true;
    } else if (argument === "--workspace") {
      if (workspace !== null) throw new Error("--workspace 不能重复");
      const value = args[index + 1];
      if (value === undefined || value.startsWith("--")) {
        throw new Error("--workspace 需要一个工作区路径");
      }
      workspace = value;
      index += 1;
    } else {
      throw new Error(`不支持的参数：${argument}`);
    }
  }
  return { help, remove, workspace };
}

// ---- I/O --------------------------------------------------------------------

/**
 * Writes the fixtures and returns the paths it wrote, for the CLI to print and
 * for a test to assert on. `directory` is mounted into the two stdio servers.
 */
export function seedFixtures(paths, directory) {
  const written = [];
  const levels = [
    {
      level: paths.global,
      skills: GLOBAL_SKILLS,
      servers: fixtureMcpServers(directory),
      // `null` means the level's file is merged instead of replaced.
      hooks: GLOBAL_HOOKS
    },
    {
      level: paths.workspace,
      skills: WORKSPACE_SKILLS,
      servers: fixtureWorkspaceMcpServers(),
      hooks: null
    }
  ];

  for (const { level, skills, servers, hooks } of levels) {
    if (level === null) continue;

    for (const skill of skills) {
      if (
        !skill.directory.startsWith(FIXTURE_SKILL_PREFIX) ||
        !skillDirectoryNameIsValid(skill.directory)
      ) {
        throw new Error(
          `夹具技能的目录名必须以 ${FIXTURE_SKILL_PREFIX} 开头，且是发现不会跳过的名字：${skill.directory}`
        );
      }
      const manifest = join(level.skills, skill.directory, SKILL_MANIFEST);
      mkdirSync(dirname(manifest), { recursive: true });
      writeFileSync(manifest, renderSkillManifest(skill), "utf8");
      written.push(manifest);
    }

    if (hooks !== null) {
      // The global hook file is replaced wholesale, so keep the original once.
      backUpOnce(level.hooks, level.hooksBackup);
      writeJsonFile(level.hooks, hooks);
    } else {
      writeJsonFile(
        level.hooks,
        mergeFixtureHooks(readJsonFile(level.hooks), WORKSPACE_HOOK_EVENT, workspaceHookHandlers())
      );
    }
    written.push(level.hooks);

    if (level.mcpBackup !== undefined) backUpOnce(level.mcp, level.mcpBackup);
    writeJsonFile(level.mcp, mergeMcpConfig(readJsonFile(level.mcp), servers));
    written.push(level.mcp);
  }
  return written;
}

/**
 * Removes the fixtures and returns the paths it changed. A non-fixture key,
 * group or file survives; a file that held nothing else is deleted.
 */
export function removeFixtures(paths) {
  const removed = [];
  removeLevelFixtures(paths.global, removed, true);
  if (paths.workspace !== null) removeLevelFixtures(paths.workspace, removed, false);
  return removed;
}

function removeLevelFixtures(level, removed, global) {
  let removedAnySkill = false;
  for (const directory of level.skillDirectories) {
    if (!existsSync(directory)) continue;
    rmSync(directory, { recursive: true, force: true });
    removed.push(directory);
    removedAnySkill = true;
  }
  // A `skills/` directory left empty by the fixtures goes with them; one the
  // user created and keeps empty is theirs and stays.
  if (removedAnySkill && existsSync(level.skills) && readdirSync(level.skills).length === 0) {
    rmSync(level.skills, { recursive: true, force: true });
  }

  if (global) {
    // The whole file was the fixture, so undo the replacement rather than
    // editing it: put the kept original back. The restore is skipped when the
    // file no longer matches the fixture, because a handler carries no marker —
    // a file the user edited after seeding, and the backup that is the only
    // copy of their original, both have to survive. With no backup, a file that
    // is exactly the fixture and nothing else is deleted.
    if (existsSync(level.hooksBackup)) {
      if (!existsSync(level.hooks) || fileEqualsValue(level.hooks, GLOBAL_HOOKS)) {
        renameSync(level.hooksBackup, level.hooks);
        removed.push(level.hooks);
      }
    } else if (fileEqualsValue(level.hooks, GLOBAL_HOOKS)) {
      rmSync(level.hooks, { force: true });
      removed.push(level.hooks);
    }
    if (existsSync(level.mcpBackup)) {
      if (existsSync(level.mcp)) {
        writeJsonFile(level.mcp, removeFixtureMcpServers(readJsonFile(level.mcp)));
        rmSync(level.mcpBackup, { force: true });
      } else {
        renameSync(level.mcpBackup, level.mcp);
      }
      removed.push(level.mcp);
      return;
    }
  } else {
    writeJsonFileOrDelete(
      level.hooks,
      mergeFixtureHooks(readJsonFile(level.hooks), WORKSPACE_HOOK_EVENT, []),
      "hooks"
    );
  }

  writeJsonFileOrDelete(level.mcp, removeFixtureMcpServers(readJsonFile(level.mcp)), "mcpServers");
}

/** Writes the document, or deletes the file when the fixtures were all it had. */
function writeJsonFileOrDelete(path, document, key) {
  if (!existsSync(path)) return;
  if (documentIsEmpty(document, key)) {
    rmSync(path, { force: true });
    return;
  }
  writeJsonFile(path, document);
}

function readJsonFile(path) {
  if (!existsSync(path)) return undefined;
  const text = readFileSync(path, "utf8");
  if (text.trim().length === 0) return undefined;
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new Error(`无法解析 ${path}：${error.message}（先修好这个文件再运行）`);
  }
}

function writeJsonFile(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

/** Keeps the first original only: a re-run must not overwrite it with fixtures. */
function backUpOnce(path, backup) {
  if (existsSync(path) && !existsSync(backup)) copyFileSync(path, backup);
}

function fileEqualsValue(path, value) {
  if (!existsSync(path)) return false;
  try {
    return jsonEquals(JSON.parse(readFileSync(path, "utf8")), value);
  } catch {
    return false;
  }
}

function jsonEquals(left, right) {
  if (left === right) return true;
  if (Array.isArray(left) && Array.isArray(right)) {
    return (
      left.length === right.length &&
      left.every((item, index) => jsonEquals(item, right[index]))
    );
  }
  if (isPlainObject(left) && isPlainObject(right)) {
    const keys = Object.keys(left);
    return (
      keys.length === Object.keys(right).length &&
      keys.every((key) => Object.hasOwn(right, key) && jsonEquals(left[key], right[key]))
    );
  }
  return false;
}

// ---- CLI --------------------------------------------------------------------

function main() {
  let options;
  try {
    options = parseFixtureArguments(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${error.message}\n\n${USAGE}\n`);
    process.exitCode = 1;
    return;
  }
  if (options.help) {
    process.stdout.write(`${USAGE}\n`);
    return;
  }

  const home = homedir();
  const workspace = options.workspace === null ? null : resolve(options.workspace);
  const paths = planFixturePaths(home, workspace);
  try {
    if (options.remove) {
      const removed = removeFixtures(paths);
      process.stdout.write(`已移除 ${removed.length} 项夹具：\n`);
      for (const path of removed) process.stdout.write(`  - ${path}\n`);
      if (workspace === null) {
        process.stdout.write("（项目级夹具需要带上同一个 --workspace <path> 才会一并移除）\n");
      }
      return;
    }
    const written = seedFixtures(paths, process.cwd());
    for (const path of written) process.stdout.write(`  + ${path}\n`);
    process.stdout.write(
      `夹具已写入 ${written.length} 个位置（全局：${join(home, CONFIG_DIRECTORY)}）\n`
    );
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}

// Importing this module from a test must not seed anything.
const invokedDirectly =
  process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (invokedDirectly) main();

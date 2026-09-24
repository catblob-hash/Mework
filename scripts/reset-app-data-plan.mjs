// Pure planning half of `npm run reset:data`. Every decision about *what* gets
// deleted lives here so it can be tested without touching the real user profile;
// scripts/reset-app-data.mjs is the thin I/O entrypoint.
//
// The app deliberately ships no backward-compatibility migrations, so a schema
// bump requires deleting the persisted document before the next launch. This
// planner enumerates every location the running app owns.

import path from "node:path";

/** Tauri `identifier` from src-tauri/tauri.conf.json — the production data directory name. */
export const PRODUCTION_IDENTIFIER = "com.mework.app";
/** Retired identifier from the CatIC/naiword era; only ever read by the legacy migration. */
export const LEGACY_IDENTIFIER = "com.naiword.agentstudio";
/** Every browser-dev data identifier carries this prefix (src-tauri/src/browser_dev.rs). */
export const DEV_IDENTIFIER_PREFIX = "com.mework.app.e2e.";
export const LEGACY_DEV_IDENTIFIER_PREFIX = "com.naiword.agentstudio.e2e.";
/** The one dev identifier that is stable across restarts (scripts/browser-dev.mjs). */
export const INTERACTIVE_DEV_IDENTIFIER = "com.mework.app.e2e.interactive-dev";

/**
 * Credential-store services the app writes to, as passed to `keyring::Entry::new`.
 * On Windows the keyring crate stores each entry as a Generic Credential whose
 * target name is `<identity>.<service>`, so a service is matched by suffix.
 */
export const KEYRING_SERVICES = [
  // Provider API keys use "com.mework.api"; "com.naiword.agent-studio.api" is a
  // legacy target retained only so reset purges its credentials. Current identities
  // are keyed by provider ID, and the app ignores older entries.
  "com.mework.api",
  "com.naiword.agent-studio.api",
  // Search-provider keys share "com.mework.api" with model-provider keys.
  // This retired service belongs to the removed bundled search feature and is
  // kept so reset purges credentials from older builds.
  "com.mework.web-search",
  // src-tauri/src/project_import_trust.rs — per-project import trust records.
  "com.mework.app.project-import-trust.v1",
  // This retired service belongs to the removed plugin marketplace. Nothing
  // writes it now, but reset purges source credentials from older builds.
  "Mework Marketplace",
  // This retired service belongs to the removed memory feature. Nothing writes
  // it now, so every surviving entry is an orphan that reset purges.
  "com.mework.memory.v1"
];

/**
 * The one login-keychain item Mework writes on macOS: the master key that seals
 * every other credential in `~/.mework/credential-vault` (src-tauri/src/credential_vault.rs).
 * Builds before that store wrote one item per credential under KEYRING_SERVICES,
 * so `--keys` plans both.
 */
export const MACOS_VAULT_KEY_SERVICE = "Mework Safe Storage";

/** Scope selectors accepted on the command line. */
export const SCOPES = ["prod", "dev", "all"];

export function parseResetArguments(args) {
  const flags = new Set(["--dev", "--prod", "--all", "--keys", "--dry-run", "--yes"]);
  const unknown = args.filter((argument) => !flags.has(argument));
  if (unknown.length > 0) {
    throw new Error(`不支持的 reset:data 参数：${unknown.join("、")}`);
  }
  if (new Set(args).size !== args.length) {
    throw new Error("reset:data 参数不能重复");
  }

  const scopeFlags = args.filter((argument) => ["--dev", "--prod", "--all"].includes(argument));
  if (scopeFlags.length > 1) {
    throw new Error(`${scopeFlags.join(" 与 ")} 不能同时使用`);
  }
  // Default matches the pain point: `npm run tauri:dev` writes the production
  // directory, and that is the one that has to go before every schema bump.
  const scope = scopeFlags.length > 0 ? scopeFlags[0].slice("--".length) : "prod";

  return {
    scope,
    keys: args.includes("--keys"),
    dryRun: args.includes("--dry-run"),
    assumeYes: args.includes("--yes")
  };
}

function isDevIdentifier(name) {
  for (const prefix of [DEV_IDENTIFIER_PREFIX, LEGACY_DEV_IDENTIFIER_PREFIX]) {
    if (name.startsWith(prefix) && name.length > prefix.length) return true;
  }
  return false;
}

function isProductionIdentifier(name) {
  return name === PRODUCTION_IDENTIFIER || name === LEGACY_IDENTIFIER;
}

/**
 * Selects the app-owned directory names inside one roaming/local AppData root.
 *
 * Only exact identifier matches are ever returned. A prefix match on
 * `com.mework.` alone would also sweep up unrelated vendors' folders, so every
 * candidate must be a known production identifier or carry a validated
 * browser-dev prefix with a non-empty suffix.
 */
export function selectIdentifiers(entries, scope) {
  if (!SCOPES.includes(scope)) throw new Error(`未知的清理范围：${scope}`);
  return entries
    .filter((name) => {
      if (isProductionIdentifier(name)) return scope === "prod" || scope === "all";
      if (isDevIdentifier(name)) return scope === "dev" || scope === "all";
      return false;
    })
    .sort();
}

/**
 * Resolves one identifier to an absolute directory, refusing anything that
 * escapes its parent. Mirrors the containment check in
 * scripts/browser-dev.mjs' cleanupOwnedDataDirectories.
 */
export function resolveDataDirectory(parent, identifier) {
  const root = path.resolve(parent);
  const target = path.resolve(root, identifier);
  // A separator of either kind is refused on every host: on macOS and Linux a
  // backslash is an ordinary filename character, so `..\\elsewhere` would
  // otherwise pass as one directory name here and mean a parent on Windows.
  if (
    /[\\/]/u.test(identifier)
    || path.dirname(target) !== root
    || path.basename(target) !== identifier
  ) {
    throw new Error(`拒绝清理越出 ${root} 的数据目录`);
  }
  return target;
}

/**
 * Builds the full deletion plan from already-listed directory entries.
 *
 * `roots` is `[{ label, directory, entries }]` — one per AppData root — so the
 * caller owns all filesystem access and this stays pure.
 */
export function planDataDirectories(roots, scope) {
  const directories = [];
  for (const { label, directory, entries } of roots) {
    if (!directory) continue;
    for (const identifier of selectIdentifiers(entries, scope)) {
      directories.push({ label, identifier, path: resolveDataDirectory(directory, identifier) });
    }
  }
  return directories;
}

/**
 * Extracts the credential target names this app owns from raw `cmdkey /list`
 * output. A target belongs to the app only when it *ends with* `.<service>`,
 * which is exactly how the keyring crate composes `<identity>.<service>`.
 *
 * Matching anchors on `LegacyGeneric:target=`, which is how `cmdkey` renders the
 * Generic Credentials the keyring crate writes, rather than on the `Target:`
 * label in front of it. That label is localized — a Chinese Windows prints
 * `目标:` — and it arrives in the console's OEM code page, so anchoring on the
 * English word made `--keys` silently plan nothing on exactly the machines that
 * had credentials to delete. Everything after the marker is the target name,
 * taken to the end of the line because a target may contain spaces.
 */
export function planCredentialTargets(cmdkeyOutput) {
  const marker = "LegacyGeneric:target=";
  const targets = [];
  const seen = new Set();
  for (const rawLine of cmdkeyOutput.split(/\r?\n/)) {
    const start = rawLine.indexOf(marker);
    if (start < 0) continue;
    const target = rawLine.slice(start + marker.length).trim();
    if (!target || seen.has(target)) continue;
    const service = KEYRING_SERVICES.find((candidate) => target.endsWith(`.${candidate}`));
    if (!service) continue;
    seen.add(target);
    targets.push({ target, service });
  }
  return targets;
}

/** Groups planned credentials by service for the confirmation summary. */
export function summarizeCredentials(targets) {
  const counts = new Map();
  for (const { service } of targets) counts.set(service, (counts.get(service) ?? 0) + 1);
  // Codepoint order, not locale order: `Mework Marketplace` and the `com.mework.*`
  // services must not reshuffle with the host locale.
  return [...counts.entries()]
    .map(([service, count]) => ({ service, count }))
    .sort((left, right) => (left.service < right.service ? -1 : left.service > right.service ? 1 : 0));
}

/**
 * Where Tauri keeps `app_data_dir`, `app_local_data_dir` and `app_cache_dir` on
 * each host, as `[{ label, directory }]` for `planDataDirectories`.
 */
export function dataRootsFor(platform, environment, home) {
  if (platform === "win32") {
    return [
      { label: "APPDATA", directory: environment.APPDATA },
      { label: "LOCALAPPDATA", directory: environment.LOCALAPPDATA }
    ];
  }
  if (!home) return [];
  if (platform === "darwin") {
    return [
      { label: "Application Support", directory: path.join(home, "Library", "Application Support") },
      { label: "Caches", directory: path.join(home, "Library", "Caches") },
      { label: "Logs", directory: path.join(home, "Library", "Logs") }
    ];
  }
  return [
    { label: "XDG_DATA_HOME", directory: environment.XDG_DATA_HOME || path.join(home, ".local", "share") },
    { label: "XDG_CONFIG_HOME", directory: environment.XDG_CONFIG_HOME || path.join(home, ".config") },
    { label: "XDG_CACHE_HOME", directory: environment.XDG_CACHE_HOME || path.join(home, ".cache") }
  ];
}

/** Whether an executable path from the process table is a Mework instance. */
export function isMeworkExecutable(executable) {
  const name = executable.trim().split(/[\\/]/u).pop() ?? "";
  return /^mework(-browser-dev)?(\.exe)?$/iu.test(name);
}

function keychainAttribute(line, name) {
  const prefix = `"${name}"<blob>=`;
  const start = line.indexOf(prefix);
  if (start < 0) return undefined;
  const value = line.slice(start + prefix.length).trim();
  if (value.startsWith("\"") && value.endsWith("\"") && value.length >= 2) {
    return value.slice(1, -1);
  }
  // Non-ASCII values are printed as hex, sometimes followed by a quoted preview.
  const hex = /^0x([0-9A-Fa-f]+)/u.exec(value);
  if (hex && hex[1].length % 2 === 0) {
    return Buffer.from(hex[1], "hex").toString("utf8");
  }
  return undefined;
}

/**
 * Extracts the generic-password items this app owns from `security dump-keychain`
 * output (attributes only — the command never prints or unlocks secrets without
 * `-d`). An item belongs to the app only when its service is exactly one of the
 * services the app writes; the account is kept so each item can be deleted by
 * its full `(service, account)` pair and nothing else.
 */
export function planKeychainItems(dumpOutput) {
  const services = new Set([...KEYRING_SERVICES, MACOS_VAULT_KEY_SERVICE]);
  const items = [];
  const seen = new Set();
  let current = null;
  const finish = () => {
    if (!current || current.class !== "genp") return;
    const { service, account } = current;
    if (service === undefined || account === undefined || !services.has(service)) return;
    const key = `${service}\u0000${account}`;
    if (seen.has(key)) return;
    seen.add(key);
    items.push({ service, account });
  };
  for (const line of dumpOutput.split(/\r?\n/u)) {
    if (line.startsWith("keychain: ")) {
      finish();
      current = {};
      continue;
    }
    if (!current) continue;
    const itemClass = /^class: "([^"]*)"/u.exec(line);
    if (itemClass) {
      current.class = itemClass[1];
      continue;
    }
    const service = keychainAttribute(line, "svce");
    if (service !== undefined) current.service = service;
    const account = keychainAttribute(line, "acct");
    if (account !== undefined) current.account = account;
  }
  finish();
  return items;
}

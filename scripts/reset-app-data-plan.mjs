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
  if (path.dirname(target) !== root || path.basename(target) !== identifier) {
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

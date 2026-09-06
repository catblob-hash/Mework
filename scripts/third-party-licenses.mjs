// Generates release notices from locked dependencies and their installed license files.
// Run with the same Node executable used to build the SEA sidecar.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const label = "[licenses:third-party]";
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const sortPackages = (a, b) => compare(a.name, b.name) || compare(a.version, b.version);
const readJson = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
const normalizeText = (text) => text.replace(/\r\n?/g, "\n").replace(/^﻿/, "");

function fail(message) {
  console.error(`${label} ${message}`);
  process.exitCode = 1;
}

export function licenseFamilies(expression) {
  if (/^SEE LICENSE IN\s/i.test(expression)) return [];
  const tokens = expression.replace(/[()]/g, " ").split(/\s+|\//).filter(Boolean);
  return [...new Set(tokens.filter((token) => !["AND", "OR", "WITH"].includes(token)))].sort(compare);
}

function person(value) {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.map(person).filter(Boolean).join("; ");
  return value?.name ? [value.name, value.email ? `<${value.email}>` : "", value.url].filter(Boolean).join(" ") : "";
}

export function extractCopyright(text, fallback, kind = "package") {
  const lines = normalizeText(text).split("\n");
  const notices = [];
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index].trim();
    // License clauses mentioning copyright are not attribution notices.
    if (!/\bcopyright\s*(?:\(c\)|©|\d{4}|by\b)/i.test(line)) continue;
    if (/\[yyyy\]|\[name of copyright owner\]/i.test(line)) continue;
    let notice = line;
    if (/copyright\s*(?:\(c\)|©)?\s*[\d,\s-]*$/i.test(line) && lines[index + 1]?.trim()) {
      notice += ` ${lines[index + 1].trim()}`;
    }
    notices.push(notice);
  }
  return [...new Set(notices)].join("; ") || person(fallback) || `no copyright line in ${kind}`;
}

export function packageLicense(pkg, identity) {
  if (typeof pkg.license === "string" && pkg.license.trim()) return pkg.license.trim();
  if (pkg.license?.type) return pkg.license.type;
  if (Array.isArray(pkg.licenses) && pkg.licenses.length) {
    const licenses = pkg.licenses.map((entry) => typeof entry === "string" ? entry : entry.type);
    if (licenses.every(Boolean)) return licenses.join(" OR ");
  }
  if (pkg.license_file) return `SEE LICENSE IN ${pkg.license_file}`;
  throw new Error(`No determinable license for ${identity}`);
}

function licenseFiles(directory, explicit) {
  const names = fs.readdirSync(directory).filter((name) => /^(?:licen[cs]e|copying)(?:$|[._-])/i.test(name));
  if (explicit) names.push(explicit);
  return [...new Set(names)].sort(compare).flatMap((name) => {
    const file = path.resolve(directory, name);
    if (!fs.existsSync(file) || !fs.statSync(file).isFile()) return [];
    return [{ name, text: normalizeText(fs.readFileSync(file, "utf8")) }];
  });
}

function sourceUrl(pkg, fallback) {
  const repository = typeof pkg.repository === "string" ? pkg.repository : pkg.repository?.url;
  return (pkg.homepage || repository || fallback).replace(/^git\+/, "").replace(/^git:\/\//, "https://");
}

function npmPackages() {
  const directory = path.join(root, "aisdk-service");
  const lock = readJson(path.join(directory, "package-lock.json"));
  if (lock.lockfileVersion < 2 || !lock.packages) throw new Error("npm lockfile must contain the packages map");
  const result = new Map();
  for (const [location, locked] of Object.entries(lock.packages).sort(([a], [b]) => compare(a, b))) {
    if (!location || locked.dev) continue;
    const name = location.split("node_modules/").at(-1);
    // These optional CLI executables are explicitly external in build.mjs, not SEA contents.
    if (name.startsWith("@anthropic-ai/claude-agent-sdk-")) continue;
    const packageDirectory = path.join(directory, location);
    const manifest = path.join(packageDirectory, "package.json");
    if (!fs.existsSync(manifest)) throw new Error(`Missing production package ${location}; run npm ci --prefix aisdk-service`);
    const pkg = readJson(manifest);
    if (pkg.name !== name || pkg.version !== locked.version) throw new Error(`Installed package differs from lockfile: ${location}`);
    const identity = `${pkg.name}@${pkg.version}`;
    const license = packageLicense(pkg, identity);
    const files = licenseFiles(packageDirectory, /^SEE LICENSE IN (.+)$/i.exec(license)?.[1]);
    const item = { name: pkg.name, version: pkg.version, license, files,
      attribution: extractCopyright(files.map((file) => file.text).join("\n"), pkg.author),
      source: sourceUrl(pkg, `https://www.npmjs.com/package/${pkg.name}/v/${pkg.version}`) };
    if (result.has(identity) && result.get(identity).license !== license) throw new Error(`Conflicting licenses for ${identity}`);
    result.set(identity, item);
  }
  return [...result.values()].sort(sortPackages);
}

export function normalDependencyIds(metadata) {
  const ids = new Set(metadata.workspace_members);
  const queue = [...ids];
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  for (const id of queue) {
    const node = nodes.get(id);
    if (!node) throw new Error(`Missing Cargo resolve node: ${id}`);
    for (const dependency of node.deps) {
      if (!dependency.dep_kinds.some((kind) => kind.kind === null || kind.kind === "normal")) continue;
      if (!ids.has(dependency.pkg)) {
        ids.add(dependency.pkg);
        queue.push(dependency.pkg);
      }
    }
  }
  for (const id of metadata.workspace_members) ids.delete(id);
  return ids;
}

function rustPackages() {
  const args = ["metadata", "--format-version", "1", "--locked"];
  const options = { cwd: path.join(root, "src-tauri"), encoding: "utf8", maxBuffer: 64 * 1024 * 1024, windowsHide: true };
  let command = spawnSync("cargo", [...args, "--offline"], options);
  if (command.error) throw command.error;
  if (command.status !== 0) {
    console.warn(`${label} Offline Cargo metadata unavailable; retrying with locked online metadata.`);
    command = spawnSync("cargo", args, options);
  }
  if (command.error || command.status !== 0) throw new Error(`cargo metadata failed: ${command.error?.message || command.stderr}`);
  const metadata = JSON.parse(command.stdout);
  const ids = normalDependencyIds(metadata);
  return metadata.packages.filter((pkg) => ids.has(pkg.id)).map((pkg) => {
    const license = packageLicense(pkg, `${pkg.name}@${pkg.version}`);
    const files = licenseFiles(path.dirname(pkg.manifest_path), pkg.license_file);
    return { name: pkg.name, version: pkg.version, license, files,
      attribution: extractCopyright(files.map((file) => file.text).join("\n"), pkg.authors, "crate"),
      source: sourceUrl(pkg, `https://crates.io/crates/${pkg.name}/${pkg.version}`) };
  }).sort(sortPackages);
}

// Require identifying grant text, not just a filename or a mention in a README.
export function textMatchesFamily(text, family) {
  const compact = text.replace(/\s+/g, " ");
  const tests = {
    MIT: /Permission is hereby granted, free of charge[\s\S]*The above copyright notice and this permission notice/i,
    "Apache-2.0": /Apache License[\s\S]*Version 2\.0[\s\S]*END OF TERMS AND CONDITIONS/i,
    "BSD-2-Clause": /Redistribution and use in source and binary forms[\s\S]*THIS SOFTWARE IS PROVIDED/i,
    "BSD-3-Clause": /Redistribution and use in source and binary forms[\s\S]*Neither the name[\s\S]*THIS SOFTWARE IS PROVIDED/i,
    ISC: /Permission to use, copy, modify, and(?:\/or)? distribute[\s\S]*THE SOFTWARE IS PROVIDED/i,
    "0BSD": /Permission to use, copy, modify, and(?:\/or)? distribute[\s\S]*THE SOFTWARE IS PROVIDED/i,
    "MPL-2.0": /Mozilla Public License[\s\S]*2\.0[\s\S]*Exhibit B/i,
    "Unicode-3.0": /UNICODE LICENSE V3[\s\S]*Permission is hereby granted/i,
    "Unicode-DFS-2016": /UNICODE, INC\. LICENSE AGREEMENT[\s\S]*Permission is hereby granted/i,
    Zlib: /provided ['"]as-is['"][\s\S]*origin of this software must not be misrepresented/i,
    "BSL-1.0": /Boost Software License[\s\S]*Version 1\.0[\s\S]*THE SOFTWARE IS PROVIDED/i,
    "CC0-1.0": /CC0 1\.0 Universal[\s\S]*Statement of Purpose[\s\S]*Limitations and Disclaimers/i,
    "MIT-0": /Permission is hereby granted, free of charge[\s\S]*THE SOFTWARE IS PROVIDED/i,
    Unlicense: /This is free and unencumbered software released into the public domain[\s\S]*THE SOFTWARE IS PROVIDED/i,
    "AFL-2.1": /Academic Free License[\s\S]*2\.1[\s\S]*Termination for Patent Action/i,
    "CDLA-Permissive-2.0": /Community Data License Agreement[\s\S]*Permissive[\s\S]*2\.0[\s\S]*No Warranty; Limitation of Liability/i,
    "LGPL-2.1-or-later": /GNU LESSER GENERAL PUBLIC LICENSE[\s\S]*Version 2\.1[\s\S]*END OF TERMS AND CONDITIONS/i,
    "LLVM-exception": /LLVM Exceptions to the Apache 2\.0 License[\s\S]*As an exception/i,
    OpenSSL: /OpenSSL License[\s\S]*Redistributions of source code[\s\S]*THIS SOFTWARE IS PROVIDED/i
  };
  if (!tests[family]?.test(compact)) return false;
  if (family === "BSD-2-Clause" && /Neither the name/i.test(compact)) return false;
  if (family === "MIT-0" && /above copyright notice and this permission notice/i.test(compact)) return false;
  if (family === "0BSD" && /copyright notice and this permission notice appear/i.test(compact)) return false;
  if (family === "ISC" && !/copyright notice and this permission notice appear/i.test(compact)) return false;
  return true;
}

function familyText(file, family) {
  let text = file.text.trim();
  // Keep the long canonical Apache text once; the exception is a separate appendix entry.
  const llvm = text.indexOf("LLVM Exceptions to the Apache 2.0 License");
  if (llvm >= 0) text = family === "LLVM-exception" ? text.slice(llvm) : text.slice(0, llvm).trim();
  if (family === "AFL-2.1") {
    const start = text.indexOf("The Academic Free License, v. 2.1:");
    if (start >= 0) text = text.slice(start);
  }
  return text;
}

const absentAlternativeTexts = {
  "MIT-0": "dunce ships only the CC0-1.0 text. Its MIT-0 alternative is not selected here; see the CC0-1.0 text below.",
  "LGPL-2.1-or-later": "r-efi ships no LGPL license file. Its LGPL alternative is not selected here; use its MIT OR Apache-2.0 alternatives, whose texts are reproduced below."
};

const escapeCell = (text) => String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/\|/g, "&#124;").replace(/[\r\n]+/g, " ");
function table(packages) {
  return ["| Name | Version | License | Copyright / attribution | Source |", "| --- | --- | --- | --- | --- |",
    ...packages.map((pkg) => {
      const commercial = pkg.name === "@anthropic-ai/claude-agent-sdk";
      const license = pkg.license + (commercial ? " — Anthropic Commercial Terms; see THIRD-PARTY-NOTICES.md" : /^SEE LICENSE IN/i.test(pkg.license) ? "; see THIRD-PARTY-NOTICES.md" : "");
      return `| ${[pkg.name, pkg.version, license, pkg.attribution, pkg.source].map(escapeCell).join(" | ")} |`;
    })].join("\n");
}
function fence(text) {
  const longest = Math.max(2, ...[...text.matchAll(/`+/g)].map((match) => match[0].length));
  const marker = "`".repeat(longest + 1);
  return `${marker}text\n${text}${text.endsWith("\n") ? "" : "\n"}${marker}`;
}

export function generate() {
  const runtimeFile = path.join(path.dirname(process.execPath), "LICENSE");
  if (!fs.existsSync(runtimeFile)) throw new Error(`Node.js LICENSE missing next to running executable: ${runtimeFile}`);
  const runtimeText = normalizeText(fs.readFileSync(runtimeFile, "utf8"));
  if (!runtimeText.trim()) throw new Error(`Node.js LICENSE is empty: ${runtimeFile}`);
  const npm = npmPackages();
  const rust = rustPackages();
  const packages = [...npm, ...rust].sort(sortPackages);
  const families = [...new Set(packages.flatMap((pkg) => licenseFamilies(pkg.license)))].sort(compare);
  const appendix = [];
  for (const family of families) {
    let chosen;
    for (const pkg of packages) {
      if (!licenseFamilies(pkg.license).includes(family)) continue;
      const file = pkg.files.find((candidate) => textMatchesFamily(candidate.text, family));
      if (file) { chosen = { pkg, file }; break; }
    }
    if (!chosen) {
      if (!absentAlternativeTexts[family]) throw new Error(`No complete license text found for ${family}`);
      const allowed = family === "MIT-0" ? ["dunce"] : ["r-efi"];
      if (packages.some((pkg) => licenseFamilies(pkg.license).includes(family) && !allowed.includes(pkg.name))) {
        throw new Error(`Missing ${family} text in a new dependency; review its license alternatives`);
      }
      appendix.push(`### ${family}\n\n${absentAlternativeTexts[family]}`);
    } else {
      appendix.push(`### ${family}\n\nSource: ${chosen.pkg.name}@${chosen.pkg.version}, ${chosen.file.name}.\n\n${fence(familyText(chosen.file, family))}`);
    }
  }
  const text = ["# Third-party licenses", "Mework is licensed under GPL-3.0-or-later. This generated inventory lists third-party components compiled into `mework.exe` and `mework-aisdk.exe`, their declared licenses, and attribution. Hand-maintained notices for vendored/copied components and commercial terms live in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).",
    "Generated by `npm run licenses:third-party` from `aisdk-service/package-lock.json`, `src-tauri/Cargo.lock`, installed package manifests/license files, and the running Node.js distribution. Run with the same Node executable used for the release build. Build tools (including esbuild, postject, TypeScript and dev-only @types/node) are excluded.",
    "The npm inventory covers locked production dependencies conservatively, including code a bundler may tree-shake. Optional `@anthropic-ai/claude-agent-sdk-*` CLI binaries are excluded because `aisdk-service/build.mjs` explicitly externalizes them; they are not shipped. The Rust inventory follows only normal dependency edges from all Cargo workspace members and excludes the workspace members themselves. Dev/build dependency edges are excluded; proc-macros reachable through normal edges may be listed even though they execute at compile time. Cargo target-specific alternatives are included conservatively, not asserted to be linked on every platform.",
    `Inventory: **${npm.length} npm packages**, **${rust.length} Rust crates**. License families/exception identifiers: ${families.map((family) => `\`${family}\``).join(", ")}.`,
    "Declared OR expressions preserve the upstream choice of alternatives; they do not require adopting every alternative. `json-schema` offers AFL-2.1 (GPL-incompatible) OR BSD-3-Clause; use the BSD alternative. The Claude Agent SDK has Anthropic Commercial Terms, not an OSS license or a GPL grant; see THIRD-PARTY-NOTICES.md for the separate component's terms. This inventory does not itself establish permission to combine or redistribute commercial components. MPL-2.0 components retain their file-level source obligations. Optional MIT-0 and LGPL texts absent from upstream packages are identified explicitly in the appendix with the available alternative chosen instead.",
    "## Node.js runtime (mework-aisdk.exe)", `Runtime version: **${process.version}**. The sidecar executable is a copy of this Node.js executable with the application SEA bundle injected. The complete adjacent Node.js LICENSE follows, including Node's MIT license and bundled-component notices (V8, OpenSSL, ICU, zlib, and others).`, fence(runtimeText),
    "## npm packages bundled into mework-aisdk.exe", table(npm),
    "## Rust crates linked into mework.exe", table(rust),
    "## License texts", "One representative upstream text is reproduced per available license family, with package-specific copyright/attribution retained in the tables. The Node.js LICENSE above remains complete and may repeat license texts independently. Exception text is reproduced separately from its base license.", ...appendix, ""].join("\n\n");
  return { text, npmCount: npm.length, rustCount: rust.length, families };
}

function main() {
  let out = path.join(root, "THIRD-PARTY-LICENSES.md");
  let check = false;
  const args = process.argv.slice(2);
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === "--check") check = true;
    else if (args[index] === "--out" && args[index + 1] && !args[index + 1].startsWith("--")) out = path.resolve(args[++index]);
    else throw new Error(`Unsupported or incomplete argument: ${args[index]}`);
  }
  const result = generate();
  if (check) {
    if (!fs.existsSync(out) || fs.readFileSync(out, "utf8") !== result.text) throw new Error(`${out} is stale; run npm run licenses:third-party${out === path.join(root, "THIRD-PARTY-LICENSES.md") ? "" : ` -- --out "${out}"`}`);
  } else fs.writeFileSync(out, result.text);
  console.log(`${label} ${check ? "Verified" : "Wrote"} ${out}: ${result.npmCount} npm packages, ${result.rustCount} Rust crates.`);
  console.log(`${label} License families: ${result.families.join(", ")}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { main(); } catch (error) { fail(error.message); }
}

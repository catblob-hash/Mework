import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { createContentSecurityPolicy } from "./vite-csp.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const STYLE_NONCE_CARRIER_ID = "mework-csp-style-nonce";

// These files are served by Rust as deliberately capable remote-site fixtures.
// They are not renderer entry documents and must not weaken the app's CSP audit.
const RUST_REMOTE_FIXTURE_PREFIX = "src-tauri/resources/";

const SHARED_POLICY = {
  "default-src": ["'none'"],
  "script-src-attr": ["'none'"],
  "font-src": ["'self'"],
  "img-src": ["'self'", "data:"],
  "style-src-attr": ["'none'"],
  "object-src": ["'none'"],
  "worker-src": ["'none'"],
  "child-src": ["'none'"],
  "media-src": ["'none'"],
  "manifest-src": ["'none'"],
  "base-uri": ["'none'"],
  "form-action": ["'none'"],
  "frame-ancestors": ["'none'"]
};

const TAURI_POLICY_EXPECTATIONS = {
  csp: {
    ...SHARED_POLICY,
    "script-src": ["'self'"],
    "connect-src": ["ipc:", "http://ipc.localhost"],
    "style-src": ["'self'"],
    "frame-src": ["'none'"]
  },
  devCsp: {
    ...SHARED_POLICY,
    "script-src": ["'self'"],
    "connect-src": [
      "ipc:",
      "http://ipc.localhost",
      "http://localhost:1420",
      "http://127.0.0.1:1420",
      "ws://localhost:1420",
      "ws://127.0.0.1:1420",
      "ws://localhost:1430",
      "ws://127.0.0.1:1430"
    ],
    "style-src": ["'self'"],
    "frame-src": ["http:", "https:"]
  }
};

const VITE_POLICY_EXPECTATION = {
  ...SHARED_POLICY,
  "script-src": ["'nonce-audit-nonce'", "'self'"],
  "connect-src": ["'self'", "ipc:", "http://ipc.localhost"],
  "style-src": ["'nonce-audit-nonce'", "'self'"],
  "frame-src": ["http:", "https:"]
};

const SOURCE_RULES = [
  ["css-text", /\bcssText\b/gu, "cssText bypasses stylesheet-based CSP"],
  ["style-attribute", /\.setAttribute\s*\(\s*(["'])style\1/gu, "setAttribute(\"style\", ...) creates an inline style"],
  ["event-attribute", /\.setAttribute\s*\(\s*(["'])on[a-z]+\1/giu, "setAttribute(\"on...\", ...) creates an inline event handler"],
  ["inner-html", /\binnerHTML\b/gu, "innerHTML parses executable markup"],
  ["outer-html", /\bouterHTML\b/gu, "outerHTML parses executable markup"],
  ["dangerous-react-html", /\bdangerouslySetInnerHTML\b/gu, "dangerouslySetInnerHTML parses executable markup"],
  ["src-doc", /\bsrcDoc\b/gu, "iframe srcDoc parses an inline document"],
  ["insert-adjacent-html", /\binsertAdjacentHTML\b/gu, "insertAdjacentHTML parses executable markup"],
  ["document-write", /\bdocument\s*\.\s*write(?:ln)?\s*\(/gu, "document.write parses executable markup"],
  ["style-element", /\.createElement\s*\(\s*(["'])style\1/gu, "createElement(\"style\") injects a stylesheet"],
  ["eval", /(?<![\w$.])eval\s*\(/gu, "eval is incompatible with a strict script CSP"],
  ["function-constructor", /(?<![\w$.])(?:new\s+)?Function\s*\(/gu, "Function construction is incompatible with a strict script CSP"]
];

function toPosix(relativePath) {
  return relativePath.split(path.sep).join("/");
}

function lineAt(source, offset) {
  return source.slice(0, offset).split("\n").length;
}

function problem(file, line, code, message) {
  return { file, line, code, message };
}

function findTagEnd(source, start) {
  let quote = "";
  for (let index = start; index < source.length; index += 1) {
    const character = source[index];
    if (quote) {
      if (character === quote) quote = "";
      continue;
    }
    if (character === '"' || character === "'") {
      quote = character;
      continue;
    }
    if (character === ">") return index;
  }
  return -1;
}

function htmlTags(source) {
  const tags = [];
  let cursor = 0;
  while (cursor < source.length) {
    const start = source.indexOf("<", cursor);
    if (start < 0) break;
    if (source.startsWith("<!--", start)) {
      const end = source.indexOf("-->", start + 4);
      cursor = end < 0 ? source.length : end + 3;
      continue;
    }
    const end = findTagEnd(source, start + 1);
    if (end < 0) break;
    const raw = source.slice(start + 1, end);
    const match = /^\s*(\/)?\s*([a-z][\w:-]*)/iu.exec(raw);
    if (match && !raw.startsWith("!") && !raw.startsWith("?")) {
      const attributesOffset = start + 1 + match[0].length;
      tags.push({
        start,
        end,
        closing: Boolean(match[1]),
        name: match[2].toLowerCase(),
        attributes: raw.slice(match[0].length),
        attributesOffset
      });
    }
    cursor = end + 1;
  }
  return tags;
}

function parseAttributes(source, baseOffset) {
  const attributes = [];
  let cursor = 0;
  while (cursor < source.length) {
    while (/\s/u.test(source[cursor] ?? "")) cursor += 1;
    if (cursor >= source.length || source[cursor] === "/") break;
    const nameStart = cursor;
    while (cursor < source.length && !/[\s=/>]/u.test(source[cursor])) cursor += 1;
    const name = source.slice(nameStart, cursor).toLowerCase();
    if (!name) {
      cursor += 1;
      continue;
    }
    while (/\s/u.test(source[cursor] ?? "")) cursor += 1;
    let value = null;
    if (source[cursor] === "=") {
      cursor += 1;
      while (/\s/u.test(source[cursor] ?? "")) cursor += 1;
      const quote = source[cursor];
      if (quote === '"' || quote === "'") {
        cursor += 1;
        const valueStart = cursor;
        while (cursor < source.length && source[cursor] !== quote) cursor += 1;
        value = source.slice(valueStart, cursor);
        if (cursor < source.length) cursor += 1;
      } else {
        const valueStart = cursor;
        while (cursor < source.length && !/[\s>]/u.test(source[cursor])) cursor += 1;
        value = source.slice(valueStart, cursor);
      }
    }
    attributes.push({ name, value, offset: baseOffset + nameStart });
  }
  return attributes;
}

function localResource(value) {
  if (
    !value
    || /[\\\u0000-\u001f\u007f]/u.test(value)
    || /^\/\//u.test(value)
    || /^[a-z][a-z\d+.-]*:/iu.test(value)
    || value.startsWith("#")
  ) return false;
  try {
    return new URL(value, "https://mework.invalid/").origin === "https://mework.invalid";
  } catch {
    return false;
  }
}

function closingTag(source, name, after) {
  const expression = new RegExp(`<\\/\\s*${name}\\s*>`, "giu");
  expression.lastIndex = after;
  return expression.exec(source);
}

function auditHtml(file, source) {
  const findings = [];
  const tags = htmlTags(source);
  let carrierCount = 0;
  for (const tag of tags) {
    if (tag.closing) continue;
    const attributes = parseAttributes(tag.attributes, tag.attributesOffset);
    const names = new Set();
    for (const attribute of attributes) {
      if (names.has(attribute.name)) {
        findings.push(problem(file, lineAt(source, attribute.offset), "duplicate-html-attribute", `duplicate ${attribute.name} attribute is forbidden`));
      }
      names.add(attribute.name);
      if (attribute.name === "style") {
        findings.push(problem(file, lineAt(source, attribute.offset), "inline-style-attribute", "style attributes are forbidden"));
      }
      if (/^on[a-z]+$/u.test(attribute.name)) {
        findings.push(problem(file, lineAt(source, attribute.offset), "inline-event-attribute", "inline event attributes are forbidden"));
      }
    }

    const attribute = (name) => attributes.find((candidate) => candidate.name === name)?.value ?? null;
    if (tag.name === "style") {
      const close = closingTag(source, "style", tag.end + 1);
      const content = close ? source.slice(tag.end + 1, close.index) : "missing closing tag";
      const exactCarrier = file === "index.html"
        && attribute("id") === STYLE_NONCE_CARRIER_ID
        && attributes.length === 1
        && !content.trim();
      if (exactCarrier) carrierCount += 1;
      else {
        findings.push(problem(file, lineAt(source, tag.start), "inline-style-tag", `only the empty #${STYLE_NONCE_CARRIER_ID} nonce carrier is allowed`));
      }
    }

    if (tag.name === "script") {
      const sourceAttribute = attribute("src");
      if (!sourceAttribute) {
        findings.push(problem(file, lineAt(source, tag.start), "inline-script-tag", "script tags must load an external src"));
      } else if (!localResource(sourceAttribute)) {
        findings.push(problem(file, lineAt(source, tag.start), "remote-script-src", "script src must be a local relative or root path"));
      }
      const close = closingTag(source, "script", tag.end + 1);
      const content = close ? source.slice(tag.end + 1, close.index) : "missing closing tag";
      if (content.trim()) {
        findings.push(problem(file, lineAt(source, tag.start), "inline-script-content", "script tags must not contain inline content"));
      }
    }

    if (tag.name === "link" && (attribute("rel") ?? "").toLowerCase().split(/\s+/u).includes("stylesheet")) {
      const href = attribute("href");
      if (!localResource(href)) {
        findings.push(problem(file, lineAt(source, tag.start), "remote-stylesheet-src", "stylesheet href must be a local relative or root path"));
      }
    }
  }
  if (file === "index.html" && carrierCount !== 1) {
    findings.push(problem(file, 1, "missing-style-nonce-carrier", `index.html must contain exactly one empty #${STYLE_NONCE_CARRIER_ID} style element`));
  }
  return findings;
}

function maskExecutableSource(source) {
  const output = Array.from(source, (character) => character === "\n" || character === "\r" ? character : " ");
  const copy = (start, end) => {
    for (let index = start; index < end; index += 1) output[index] = source[index];
  };

  const scanString = (start, quote) => {
    let cursor = start + 1;
    let escaped = false;
    while (cursor < source.length) {
      const character = source[cursor];
      if (escaped) escaped = false;
      else if (character === "\\") escaped = true;
      else if (character === quote) {
        const raw = source.slice(start + 1, cursor);
        if (/^(?:style|on[a-z]+)$/iu.test(raw)) copy(start, cursor + 1);
        return cursor + 1;
      }
      cursor += 1;
    }
    return cursor;
  };

  const scanTemplate = (start) => {
    let cursor = start + 1;
    while (cursor < source.length) {
      if (source[cursor] === "\\") {
        cursor += 2;
        continue;
      }
      if (source[cursor] === "`") return cursor + 1;
      if (source[cursor] === "$" && source[cursor + 1] === "{") {
        copy(cursor, cursor + 2);
        cursor = scanCode(cursor + 2, true);
        continue;
      }
      cursor += 1;
    }
    return cursor;
  };

  const scanCode = (start, templateExpression = false) => {
    let cursor = start;
    let braceDepth = templateExpression ? 1 : 0;
    while (cursor < source.length) {
      if (source.startsWith("//", cursor)) {
        const end = source.indexOf("\n", cursor + 2);
        cursor = end < 0 ? source.length : end;
        continue;
      }
      if (source.startsWith("/*", cursor)) {
        const end = source.indexOf("*/", cursor + 2);
        cursor = end < 0 ? source.length : end + 2;
        continue;
      }
      const character = source[cursor];
      if (character === '"' || character === "'") {
        cursor = scanString(cursor, character);
        continue;
      }
      if (character === "`") {
        cursor = scanTemplate(cursor);
        continue;
      }
      copy(cursor, cursor + 1);
      if (templateExpression) {
        if (character === "{") braceDepth += 1;
        if (character === "}") {
          braceDepth -= 1;
          if (braceDepth === 0) return cursor + 1;
        }
      }
      cursor += 1;
    }
    return cursor;
  };

  scanCode(0);
  return output.join("");
}

function auditSource(file, source) {
  const findings = [];
  const executable = maskExecutableSource(source);
  for (const [code, expression, message] of SOURCE_RULES) {
    expression.lastIndex = 0;
    for (const match of executable.matchAll(expression)) {
      findings.push(problem(file, lineAt(source, match.index), code, message));
    }
  }
  return findings;
}

function parsePolicy(policy) {
  const entries = typeof policy === "string"
    ? policy.split(";").map((entry) => entry.trim()).filter(Boolean).map((entry) => {
        const [directive, ...sources] = entry.split(/\s+/u);
        return [directive, sources];
      })
    : policy && typeof policy === "object"
      ? Object.entries(policy).map(([directive, value]) => [
          directive,
          Array.isArray(value) ? value.flatMap((item) => String(item).trim().split(/\s+/u)) : String(value ?? "").trim().split(/\s+/u)
        ])
      : [];
  const directives = new Map();
  const duplicates = [];
  for (const [rawDirective, rawSources] of entries) {
    const directive = String(rawDirective ?? "").toLowerCase();
    if (!directive) continue;
    const sources = rawSources.filter(Boolean);
    if (directives.has(directive)) {
      duplicates.push(directive);
      continue;
    }
    directives.set(directive, sources);
  }
  return { directives, duplicates };
}

function sameSources(actual, expected) {
  return actual.length === expected.length
    && new Set(actual).size === actual.length
    && actual.every((source) => expected.includes(source));
}

export function auditPolicy(file, name, rawPolicy, expectedPolicy) {
  const findings = [];
  const { directives, duplicates } = parsePolicy(rawPolicy);
  for (const directive of duplicates) {
    findings.push(problem(file, 1, `duplicate-${directive}`, `${name} contains duplicate ${directive}; browsers enforce only the first copy`));
  }
  for (const [directive, expected] of Object.entries(expectedPolicy)) {
    const actual = directives.get(directive);
    if (!actual) {
      findings.push(problem(file, 1, `missing-${directive}`, `${name} must explicitly define ${directive}`));
      continue;
    }
    if (!sameSources(actual, expected)) {
      findings.push(problem(file, 1, `sources-${directive}`, `${name} ${directive} must contain exactly: ${expected.join(" ")}`));
    }
  }
  for (const [directive, sources] of directives) {
    if (!(directive in expectedPolicy)) {
      findings.push(problem(file, 1, `unexpected-${directive}`, `${name} contains unreviewed directive ${directive}`));
    }
    const normalized = sources.join(" ").toLowerCase();
    if (/['"]unsafe-(?:inline|eval)['"]/u.test(normalized)) {
      findings.push(problem(file, 1, `unsafe-${directive}`, `${name} ${directive} must not allow unsafe-inline or unsafe-eval`));
    }
    if (sources.some((source) => source.includes("*"))) {
      findings.push(problem(file, 1, `wildcard-${directive}`, `${name} ${directive} must not contain wildcard sources`));
    }
  }
  return findings;
}

function auditTauriConfig(file, source) {
  let config;
  try {
    config = JSON.parse(source);
  } catch (error) {
    return [problem(file, 1, "invalid-tauri-json", `cannot parse Tauri configuration: ${error.message}`)];
  }
  const security = config.app?.security;
  const findings = [];
  for (const name of ["csp", "devCsp"]) {
    if (!security?.[name]) {
      findings.push(problem(file, 1, `missing-${name}`, `app.security.${name} is required`));
      continue;
    }
    findings.push(...auditPolicy(file, name, security[name], TAURI_POLICY_EXPECTATIONS[name]));
  }
  return findings;
}

function includedHtml(relativePath) {
  return relativePath.endsWith(".html")
    && !relativePath.startsWith(RUST_REMOTE_FIXTURE_PREFIX);
}

function includedSource(relativePath) {
  return /\.tsx?$/u.test(relativePath)
    && relativePath.startsWith("src/")
    && !relativePath.includes("/test/")
    && !/\.test\.tsx?$/u.test(relativePath);
}

function sortFindings(findings) {
  return findings.sort((left, right) =>
    left.file.localeCompare(right.file)
    || left.line - right.line
    || left.code.localeCompare(right.code));
}

export function auditFrontendCsp(files) {
  const findings = [];
  for (const [file, source] of Object.entries(files)) {
    const relativePath = file.replaceAll("\\", "/");
    if (includedHtml(relativePath)) findings.push(...auditHtml(relativePath, source));
    if (includedSource(relativePath)) findings.push(...auditSource(relativePath, source));
    if (relativePath === "src-tauri/tauri.conf.json") findings.push(...auditTauriConfig(relativePath, source));
  }
  return sortFindings(findings);
}

function collectFiles(directory, files) {
  if (!fs.existsSync(directory)) return;
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const absolutePath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === "dist" || entry.name === ".git") continue;
      collectFiles(absolutePath, files);
      continue;
    }
    const relativePath = toPosix(path.relative(ROOT, absolutePath));
    if (includedHtml(relativePath) || includedSource(relativePath) || relativePath === "src-tauri/tauri.conf.json") {
      files[relativePath] = fs.readFileSync(absolutePath, "utf8");
    }
  }
}

export function auditWorkspace(root = ROOT) {
  const files = {};
  if (fs.existsSync(root)) {
    for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
      if (!entry.isFile() || !entry.name.endsWith(".html")) continue;
      files[entry.name] = fs.readFileSync(path.join(root, entry.name), "utf8");
    }
  }
  collectFiles(path.join(root, "src"), files);
  const tauriConfig = path.join(root, "src-tauri", "tauri.conf.json");
  if (fs.existsSync(tauriConfig)) {
    files["src-tauri/tauri.conf.json"] = fs.readFileSync(tauriConfig, "utf8");
  }
  const findings = auditFrontendCsp(files);
  findings.push(...auditPolicy(
    "scripts/vite-csp.mjs",
    "Vite response CSP",
    createContentSecurityPolicy("audit-nonce", []),
    VITE_POLICY_EXPECTATION
  ));
  return sortFindings(findings);
}

export function formatFindings(findings) {
  return findings.map(({ file, line, code, message }) => `${file}:${line} [${code}] ${message}`).join("\n");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const findings = auditWorkspace();
  if (findings.length) {
    console.error("Frontend CSP source audit failed:");
    console.error(formatFindings(findings));
    process.exitCode = 1;
  } else {
    console.log("Frontend CSP source audit passed.");
  }
}

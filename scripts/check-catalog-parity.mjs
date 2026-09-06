import fs from "node:fs";
import path from "node:path";

// The Rust tool catalog (src-tauri/src/catalog.rs `tool_catalog()`) and its hand-maintained
// TypeScript mirror (src/seed.ts `toolCatalog`) have to agree, and nothing else in the repo
// reconciles them: toolDefaults.test.ts imports only ../seed, and check-command-acl.mjs covers
// Tauri commands rather than model tools. A Rust-only tool addition therefore passes `npm test`
// silently and then `normalizeDocument` (src/lib/runtime.ts) drops the unknown tool from every
// persisted enabledTools list. A half-done SCHEMA_VERSION bump is worse: seed.ts falling behind
// storage.rs makes runtime.ts reject documents written by newer versions on every load.
//
// Tool ORDER is deliberately not compared. The two catalogs hold the same names in different
// order today (seed.ts groups Media earlier), and order is cosmetic — normalizeDocument rebuilds
// document.tools from the seed regardless.
//
// ---------------------------------------------------------------------------
// AUTHORITATIVE CHECKLIST — adding one tool to the catalog touches all of this.
// Anchor on function names, not line numbers: catalog.rs is refactored often.
//
//  1. `descriptor(...)` inside `tool_catalog()`. `parameter()` takes SEVEN args, in order:
//     name, label, parameter_type, required, default_value, placeholder, help.
//  2. `english_tool_default` arm — MANDATORY, the localizer panics without it.
//  3. `english_parameter_label` arm per new parameter — MANDATORY.
//  4. `english_parameter_help` / `english_parameter_placeholder` arms — only when the Chinese
//     value contains Han characters (the panic is guarded on that).
//  5. `security::classify_model_call` internal match arm — MANDATORY. Without it every call
//     dies at the exhaustive unknown-tool classifier fallback.
//  6. `security::classify` manual-execution rejection arm — optional, message quality only.
//  7. Dispatch: the api.rs orchestration ladder when ToolCategory is Orchestration/Agent,
//     otherwise the tool_executor.rs generic path (else it fails as an unknown tool).
//  8. An explicit include/exclude decision in `SUBAGENT_DISABLED_TOOL_NAMES` (api.rs).
//  9. `tool_schema` (protocol_adapter/chat/tools.rs) only when the parameter-derived schema is
//     insufficient. A tool whose real schema comes from a runtime binding (the three
//     `generate_*` tools) still needs a hand-written static one in builtin_schemas.rs, because the
//     structural suite there walks the whole catalog — plus a test proving that static shape never
//     reaches the wire.
// 10. The catalog length assertion in catalog.rs
//     (`english_tool_catalog_localizes_every_visible_default_without_mutating_chinese`). That is
//     the ONLY whole-catalog length assertion in Rust today. security.rs and browser.rs each count
//     `playwright` alone and do not move with the catalog size.
// 11. `SCHEMA_VERSION` in storage.rs. This repo writes NO migration for it: the app is
//     unreleased, so an older document is refused rather than upgraded, and stale data is
//     cleared with `npm run reset:data`.
// 12. `src/seed.ts` descriptor AND `schemaVersion`, in the SAME commit.
// 13. `TOOL_VIEW_REGISTRY` in src/components/ToolRenderers.tsx.
// 14. `src/lib/toolDefaults.ts`: englishToolDefaults plus the per-parameter maps.
// 15. The generated design snapshots: `node scripts/export-context-injections.mjs` (four files),
//     and the Rust golden `cargo test --lib -- builtin_schemas::tests::regenerate_builtin_schema_baseline --ignored`.
//     Neither is an npm script; both have a `--check`/freshness counterpart that fails otherwise.
//
// Steps 10 and 12 are what this guard enforces. The rest fail as panics or count assertions.
// ---------------------------------------------------------------------------

const root = process.cwd();
const catalogPath = path.join(root, "src-tauri", "src", "catalog.rs");
const storagePath = path.join(root, "src-tauri", "src", "storage.rs");
const seedPath = path.join(root, "src", "seed.ts");

const rel = (target) => path.relative(root, target).replaceAll("\\", "/");
const errors = [];

/** Slice from `open` at `start` to its matching close, inclusive, skipping string literals. */
function sliceBalanced(source, start, open, close) {
  let depth = 0;
  let index = start;
  while (index < source.length) {
    const char = source[index];
    if (char === '"') {
      index += 1;
      while (index < source.length && source[index] !== '"') {
        index += source[index] === "\\" ? 2 : 1;
      }
    } else if (char === open) {
      depth += 1;
    } else if (char === close) {
      depth -= 1;
      if (depth === 0) return source.slice(start, index + 1);
    }
    index += 1;
  }
  throw new Error(`unbalanced ${open}${close} starting at offset ${start}`);
}

/** Split a Rust argument list on top-level commas, respecting nesting and strings. */
function splitArguments(body) {
  const parts = [];
  let current = "";
  let depth = 0;
  let index = 0;
  while (index < body.length) {
    const char = body[index];
    if (char === '"') {
      let literal = char;
      index += 1;
      while (index < body.length && body[index] !== '"') {
        if (body[index] === "\\") {
          literal += body[index] + (body[index + 1] ?? "");
          index += 2;
          continue;
        }
        literal += body[index];
        index += 1;
      }
      current += `${literal}"`;
      index += 1;
      continue;
    }
    if (char === "(" || char === "[" || char === "{") depth += 1;
    if (char === ")" || char === "]" || char === "}") depth -= 1;
    if (char === "," && depth === 0) {
      parts.push(current.trim());
      current = "";
      index += 1;
      continue;
    }
    current += char;
    index += 1;
  }
  if (current.trim()) parts.push(current.trim());
  return parts;
}

const stringLiteral = (text) => {
  const match = /^"((?:[^"\\]|\\.)*)"$/su.exec(text.trim());
  return match ? match[1] : null;
};

// --- Rust side -------------------------------------------------------------

const catalogSource = fs.readFileSync(catalogPath, "utf8");
const catalogFnIndex = catalogSource.indexOf("pub fn tool_catalog() -> Vec<ToolDescriptor> {");
if (catalogFnIndex < 0) {
  console.error(`${rel(catalogPath)}: could not find \`pub fn tool_catalog()\`.`);
  process.exit(1);
}
const catalogBody = sliceBalanced(
  catalogSource,
  catalogSource.indexOf("{", catalogFnIndex),
  "{",
  "}"
);

// `ToolParameterType` variants are imported unqualified inside tool_catalog(), and `String` is
// aliased to StringType to avoid shadowing the prelude. Helper functions outside that scope use
// the qualified form.
const parameterTypeNames = {
  StringType: "string",
  "ToolParameterType::String": "string",
  Number: "number",
  "ToolParameterType::Number": "number",
  Boolean: "boolean",
  "ToolParameterType::Boolean": "boolean",
  Multiline: "multiline",
  "ToolParameterType::Multiline": "multiline",
  Json: "json",
  "ToolParameterType::Json": "json",
};

// Descriptors whose parameter expression is not statically analysable. Each entry is a known,
// reviewed hole: its parameter list is NOT compared against seed.ts. Keep this list empty where
// possible — prefer inlining `vec![parameter(...)]` — and never add an entry to silence a real
// drift report.
const UNRESOLVED_PARAMETER_TOOLS = new Set();

/** Parse every `parameter(...)` call in `body`, resolving identifiers through `bindings`. */
function parseParameterCalls(body, bindings = new Map()) {
  const resolved = [];
  for (
    let cursor = body.indexOf("parameter(");
    cursor >= 0;
    cursor = body.indexOf("parameter(", cursor + 1)
  ) {
    const previous = body[cursor - 1] ?? "";
    if (/[A-Za-z0-9_]/u.test(previous)) continue;
    const args = splitArguments(
      sliceBalanced(body, body.indexOf("(", cursor), "(", ")").slice(1, -1)
    );
    if (args.length !== 7) continue;
    const name = stringLiteral(args[0]) ?? bindings.get(args[0].trim()) ?? null;
    if (name === null) continue;
    const requiredText = args[3].trim();
    resolved.push({
      name,
      type: parameterTypeNames[args[2].trim()] ?? args[2].trim(),
      required: (bindings.get(requiredText) ?? requiredText) === "true",
    });
  }
  return resolved;
}

// Helper functions that return a parameter vector, keyed by name, so a descriptor delegating to
// one can still be compared. Signature parameter names map positionally onto the call arguments.
const parameterHelpers = new Map();
for (const match of catalogSourceHelpers()) parameterHelpers.set(match.name, match);

function* catalogSourceHelpers() {
  const source = fs.readFileSync(catalogPath, "utf8");
  const pattern = /fn\s+([a-z_][a-z0-9_]*)\s*\(([^)]*)\)\s*->\s*Vec<ToolParameter>\s*\{/gu;
  for (const match of source.matchAll(pattern)) {
    const signature = match[2]
      .split(",")
      .map((entry) => entry.trim())
      .filter(Boolean)
      .map((entry) => entry.split(":")[0].trim());
    const body = sliceBalanced(source, match.index + match[0].length - 1, "{", "}");
    yield { name: match[1], signature, body };
  }
}

const rustTools = [];
for (let index = catalogBody.indexOf("descriptor("); index >= 0; index = catalogBody.indexOf("descriptor(", index + 1)) {
  // `parameter(` and `descriptor(` both end in "tor(" / "ter(" — anchor on a call boundary so a
  // substring inside an identifier cannot match.
  const before = catalogBody[index - 1] ?? "";
  if (/[A-Za-z0-9_]/u.test(before)) continue;
  const args = splitArguments(
    sliceBalanced(catalogBody, catalogBody.indexOf("(", index), "(", ")").slice(1, -1)
  );
  if (args.length !== 6) continue;
  const name = stringLiteral(args[0]);
  const category = /^ToolCategory::([A-Za-z]+)$/u.exec(args[3]);
  if (name === null || !category) continue;

  const parameterExpression = args[5].trim();
  let parameters = null;
  if (parameterExpression === "Vec::new()" || /^vec!\[\s*\]$/u.test(parameterExpression)) {
    parameters = [];
  } else if (parameterExpression.startsWith("vec![")) {
    parameters = parseParameterCalls(parameterExpression);
  } else {
    const helperCall = /^([a-z_][a-z0-9_]*)\s*\(/u.exec(parameterExpression);
    const helper = helperCall ? parameterHelpers.get(helperCall[1]) : undefined;
    if (helper) {
      const callArguments = splitArguments(
        sliceBalanced(parameterExpression, parameterExpression.indexOf("("), "(", ")").slice(1, -1)
      );
      const bindings = new Map(
        helper.signature.map((parameterName, position) => [
          parameterName,
          stringLiteral(callArguments[position] ?? "") ?? (callArguments[position] ?? "").trim(),
        ])
      );
      parameters = parseParameterCalls(helper.body, bindings);
    }
  }

  if (parameters === null && !UNRESOLVED_PARAMETER_TOOLS.has(name)) {
    errors.push(
      `${rel(catalogPath)}: tool "${name}" builds its parameters with an expression this guard cannot analyse. Inline \`vec![parameter(...)]\`, delegate to a \`-> Vec<ToolParameter>\` helper, or add it to UNRESOLVED_PARAMETER_TOOLS with a reason.`
    );
  }

  rustTools.push({
    name,
    category: category[1].toLowerCase(),
    dangerous: args[4].trim() === "true",
    parameters,
  });
}

if (!rustTools.length) {
  console.error(
    `${rel(catalogPath)}: parsed zero descriptors from tool_catalog(). The parser is stale — fix it rather than skipping the check.`
  );
  process.exit(1);
}

// --- TypeScript side -------------------------------------------------------

const seedSource = fs.readFileSync(seedPath, "utf8");
const seedDeclaration = seedSource.indexOf("export const toolCatalog");
if (seedDeclaration < 0) {
  console.error(`${rel(seedPath)}: could not find \`export const toolCatalog\`.`);
  process.exit(1);
}
// Start after the `=`, not after the declaration — the annotation `: ToolDescriptor[]` carries
// its own bracket pair.
const seedAssignment = seedSource.indexOf("=", seedDeclaration);
const seedArrayText = sliceBalanced(seedSource, seedSource.indexOf("[", seedAssignment), "[", "]");
let seedTools;
try {
  // The literal is pure data — no type annotations, no calls. Evaluating it beats a second
  // hand-rolled parser that would drift from the first.
  seedTools = new Function(`"use strict"; return ${seedArrayText};`)();
} catch (error) {
  console.error(`${rel(seedPath)}: could not evaluate the toolCatalog literal: ${error.message}`);
  process.exit(1);
}
if (!Array.isArray(seedTools) || !seedTools.length || typeof seedTools[0]?.name !== "string") {
  console.error(`${rel(seedPath)}: toolCatalog did not evaluate to a non-empty descriptor array.`);
  process.exit(1);
}

// --- Diff ------------------------------------------------------------------

const rustByName = new Map(rustTools.map((tool) => [tool.name, tool]));
const seedByName = new Map(seedTools.map((tool) => [tool.name, tool]));

for (const name of rustByName.keys()) {
  if (!seedByName.has(name)) {
    errors.push(`${rel(seedPath)}: missing tool "${name}" present in tool_catalog()`);
  }
}
for (const name of seedByName.keys()) {
  if (!rustByName.has(name)) {
    errors.push(`${rel(catalogPath)}: missing tool "${name}" present in seed.ts toolCatalog`);
  }
}

for (const [name, rustTool] of rustByName) {
  const seedTool = seedByName.get(name);
  if (!seedTool) continue;
  if (rustTool.category !== seedTool.category) {
    errors.push(`tool "${name}": category ${rustTool.category} (rust) vs ${seedTool.category} (seed)`);
  }
  if (rustTool.dangerous !== Boolean(seedTool.dangerous)) {
    errors.push(
      `tool "${name}": dangerous ${rustTool.dangerous} (rust) vs ${Boolean(seedTool.dangerous)} (seed)`
    );
  }
  if (rustTool.parameters === null) continue;
  const rustParameters = rustTool.parameters.map((parameter) => parameter.name).join(", ");
  const seedParameters = (seedTool.parameters ?? []).map((parameter) => parameter.name).join(", ");
  if (rustParameters !== seedParameters) {
    errors.push(`tool "${name}": parameters [${rustParameters}] (rust) vs [${seedParameters}] (seed)`);
  }
  for (const rustParameter of rustTool.parameters) {
    const seedParameter = (seedTool.parameters ?? []).find((entry) => entry.name === rustParameter.name);
    if (!seedParameter) continue;
    if (rustParameter.type !== seedParameter.type) {
      errors.push(
        `tool "${name}" parameter "${rustParameter.name}": type ${rustParameter.type} (rust) vs ${seedParameter.type} (seed)`
      );
    }
    if (rustParameter.required !== Boolean(seedParameter.required)) {
      errors.push(
        `tool "${name}" parameter "${rustParameter.name}": required ${rustParameter.required} (rust) vs ${Boolean(seedParameter.required)} (seed)`
      );
    }
  }
}

// --- SCHEMA_VERSION lockstep ----------------------------------------------

const storageSource = fs.readFileSync(storagePath, "utf8");
const rustSchemaVersion = /const\s+SCHEMA_VERSION\s*:\s*u32\s*=\s*(\d+)\s*;/u.exec(storageSource);
const seedSchemaVersion = /\bschemaVersion\s*:\s*(\d+)/u.exec(seedSource);
if (!rustSchemaVersion) {
  console.error(`${rel(storagePath)}: could not find \`const SCHEMA_VERSION: u32\`.`);
  process.exit(1);
}
if (!seedSchemaVersion) {
  console.error(`${rel(seedPath)}: could not find \`schemaVersion\`.`);
  process.exit(1);
}
if (rustSchemaVersion[1] !== seedSchemaVersion[1]) {
  errors.push(
    `SCHEMA_VERSION ${rustSchemaVersion[1]} (${rel(storagePath)}) != schemaVersion ${seedSchemaVersion[1]} (${rel(seedPath)}) — a document written by the new backend fails to load in the renderer.`
  );
}

if (errors.length) {
  errors.forEach((message) => console.error(message));
  process.exitCode = 1;
} else {
  console.log(
    `catalog parity ok: ${rustTools.length} tools, schemaVersion ${rustSchemaVersion[1]}`
  );
}

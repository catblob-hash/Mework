import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const catalogPath = join(root, "src-tauri", "src", "catalog.rs");
const outputDirectory = join(root, "docs", "context-injections");
const schemaBaselinePath = join(outputDirectory, "builtin-tool-schemas.json");
const checkOnly = process.argv.includes("--check");

const parameterTypes = new Map([
  ["StringType", "string"],
  ["Number", "number"],
  ["Boolean", "boolean"],
  ["Multiline", "multiline"],
  ["Json", "json"]
]);

function rustStringEnd(source, start) {
  if (source[start] === '"') {
    let index = start + 1;
    while (index < source.length) {
      if (source[index] === "\\") index += 2;
      else if (source[index] === '"') return index + 1;
      else index += 1;
    }
    throw new Error(`unterminated Rust string at offset ${start}`);
  }
  if (source[start] !== "r") return null;
  let quote = start + 1;
  while (source[quote] === "#") quote += 1;
  if (source[quote] !== '"') return null;
  const hashes = source.slice(start + 1, quote);
  const close = `"${hashes}`;
  const end = source.indexOf(close, quote + 1);
  if (end < 0) throw new Error(`unterminated Rust raw string at offset ${start}`);
  return end + close.length;
}

function skipRustTrivia(source, start) {
  const stringEnd = rustStringEnd(source, start);
  if (stringEnd !== null) return stringEnd;
  if (source.startsWith("//", start)) {
    const end = source.indexOf("\n", start + 2);
    return end < 0 ? source.length : end + 1;
  }
  if (source.startsWith("/*", start)) {
    let depth = 1;
    let index = start + 2;
    while (index < source.length && depth > 0) {
      if (source.startsWith("/*", index)) {
        depth += 1;
        index += 2;
      } else if (source.startsWith("*/", index)) {
        depth -= 1;
        index += 2;
      } else {
        index += 1;
      }
    }
    if (depth !== 0) throw new Error(`unterminated Rust block comment at offset ${start}`);
    return index;
  }
  return null;
}

function sliceBalanced(source, start, open, close) {
  let depth = 0;
  let index = start;
  while (index < source.length) {
    const skipped = skipRustTrivia(source, index);
    if (skipped !== null) {
      index = skipped;
      continue;
    }
    if (source[index] === open) depth += 1;
    else if (source[index] === close) {
      depth -= 1;
      if (depth === 0) return source.slice(start, index + 1);
    }
    index += 1;
  }
  throw new Error(`unbalanced ${open}${close} starting at offset ${start}`);
}

function splitTopLevel(source, separator = ",") {
  const parts = [];
  let depth = 0;
  let start = 0;
  let index = 0;
  while (index < source.length) {
    const skipped = skipRustTrivia(source, index);
    if (skipped !== null) {
      index = skipped;
      continue;
    }
    const character = source[index];
    if ("([{".includes(character)) depth += 1;
    else if (")]}".includes(character)) depth -= 1;
    else if (character === separator && depth === 0) {
      const part = source.slice(start, index).trim();
      if (part) parts.push(part);
      start = index + 1;
    }
    index += 1;
  }
  const tail = source.slice(start).trim();
  if (tail) parts.push(tail);
  return parts;
}

function parseRustString(token) {
  const value = token.trim();
  const raw = /^r(#+)?"([\s\S]*)"\1$/u.exec(value);
  if (raw) return raw[2];
  if (!value.startsWith('"') || !value.endsWith('"')) return null;
  try {
    return JSON.parse(value);
  } catch (error) {
    throw new Error(`unsupported Rust string ${value}: ${error.message}`);
  }
}

function unwrapCall(token, name) {
  const value = token.trim();
  const prefix = `${name}(`;
  if (!value.startsWith(prefix)) return null;
  const body = sliceBalanced(value, value.indexOf("("), "(", ")");
  if (body.length !== value.length - name.length) return null;
  return body.slice(1, -1).trim();
}

function parseOptionalString(token) {
  if (token.trim() === "None") return undefined;
  const body = unwrapCall(token, "Some");
  assert.notEqual(body, null, `unsupported Option<String>: ${token}`);
  const arguments_ = splitTopLevel(body);
  assert.equal(arguments_.length, 1, `unsupported Option<String> arity: ${token}`);
  const value = parseRustString(arguments_[0]);
  assert.notEqual(value, null, `unsupported Option<String> literal: ${token}`);
  return value;
}

function parseDefaultValue(token) {
  if (token.trim() === "None") return undefined;
  const some = unwrapCall(token, "Some");
  assert.notEqual(some, null, `unsupported default Option: ${token}`);
  const arguments_ = splitTopLevel(some);
  assert.equal(arguments_.length, 1, `unsupported default Option arity: ${token}`);
  const json = unwrapCall(arguments_[0], "json!");
  assert.notEqual(json, null, `unsupported default value: ${token}`);
  const string = parseRustString(json);
  if (string !== null) return string;
  try {
    return JSON.parse(json);
  } catch (error) {
    throw new Error(`unsupported json! default ${json}: ${error.message}`);
  }
}

function functionBody(source, signature) {
  const start = source.indexOf(signature);
  assert.notEqual(start, -1, `could not find ${signature}`);
  const brace = source.indexOf("{", start + signature.length);
  return sliceBalanced(source, brace, "{", "}").slice(1, -1);
}

function parseParameters(expression) {
  const parameters = [];
  for (
    let cursor = expression.indexOf("parameter(");
    cursor >= 0;
    cursor = expression.indexOf("parameter(", cursor + 1)
  ) {
    const previous = expression[cursor - 1] ?? "";
    if (/[A-Za-z0-9_]/u.test(previous)) continue;
    const call = sliceBalanced(expression, expression.indexOf("(", cursor), "(", ")");
    const args = splitTopLevel(call.slice(1, -1));
    if (args.length !== 7) continue;
    const name = parseRustString(args[0]);
    const label = parseRustString(args[1]);
    const type = parameterTypes.get(args[2].trim());
    assert.ok(name && label && type, `could not parse parameter call: ${call}`);
    const parameter = {
      name,
      label,
      type,
      required: args[3].trim() === "true"
    };
    const defaultValue = parseDefaultValue(args[4]);
    const placeholder = parseOptionalString(args[5]);
    const help = parseOptionalString(args[6]);
    if (defaultValue !== undefined) parameter.defaultValue = defaultValue;
    if (placeholder !== undefined) parameter.placeholder = placeholder;
    if (help !== undefined) parameter.help = help;
    parameters.push(parameter);
  }
  return parameters;
}

function parseChineseCatalog(source) {
  const body = functionBody(source, "pub fn tool_catalog() -> Vec<ToolDescriptor>");
  const tools = [];
  for (
    let cursor = body.indexOf("descriptor(");
    cursor >= 0;
    cursor = body.indexOf("descriptor(", cursor + 1)
  ) {
    const previous = body[cursor - 1] ?? "";
    if (/[A-Za-z0-9_]/u.test(previous)) continue;
    const call = sliceBalanced(body, body.indexOf("(", cursor), "(", ")");
    const args = splitTopLevel(call.slice(1, -1));
    if (args.length !== 6) continue;
    const name = parseRustString(args[0]);
    const label = parseRustString(args[1]);
    const description = parseRustString(args[2]);
    const category = /^ToolCategory::([A-Za-z]+)$/u.exec(args[3].trim())?.[1].toLowerCase();
    // Descriptions may be empty because usage-layer seeds remain empty.
    assert.ok(name && label && description !== null && category,
      `could not parse descriptor: ${call}`);
    tools.push({
      name,
      label,
      description,
      category,
      dangerous: args[4].trim() === "true",
      parameters: parseParameters(args[5])
    });
  }
  assert.ok(tools.length > 0, "parsed no tools from tool_catalog");
  assert.equal(new Set(tools.map((tool) => tool.name)).size, tools.length,
    "tool_catalog contains duplicate names");
  return tools;
}

function splitMatchArms(source, functionSignature) {
  const body = functionBody(source, functionSignature);
  const matchStart = body.indexOf("match");
  assert.notEqual(matchStart, -1, `could not find match in ${functionSignature}`);
  const brace = body.indexOf("{", matchStart);
  const matchBody = sliceBalanced(body, brace, "{", "}").slice(1, -1);
  const arms = [];
  let index = 0;
  while (index < matchBody.length) {
    while (index < matchBody.length && /[\s,]/u.test(matchBody[index])) index += 1;
    if (matchBody.startsWith("//", index) || matchBody.startsWith("/*", index)) {
      index = skipRustTrivia(matchBody, index);
      continue;
    }
    if (index >= matchBody.length) break;
    const patternStart = index;
    let depth = 0;
    let arrow = -1;
    while (index < matchBody.length - 1) {
      const skipped = skipRustTrivia(matchBody, index);
      if (skipped !== null) {
        index = skipped;
        continue;
      }
      if ("([{".includes(matchBody[index])) depth += 1;
      else if (")]}".includes(matchBody[index])) depth -= 1;
      else if (depth === 0 && matchBody.startsWith("=>", index)) {
        arrow = index;
        break;
      }
      index += 1;
    }
    assert.notEqual(arrow, -1, `could not find match arm arrow in ${functionSignature}`);
    index = arrow + 2;
    while (/\s/u.test(matchBody[index] ?? "")) index += 1;
    const expressionStart = index;
    const stringEnd = rustStringEnd(matchBody, expressionStart);
    if (stringEnd !== null) index = stringEnd;
    else if (matchBody[expressionStart] === "{") {
      index = expressionStart + sliceBalanced(matchBody, expressionStart, "{", "}").length;
    } else if (matchBody[expressionStart] === "(") {
      index = expressionStart + sliceBalanced(matchBody, expressionStart, "(", ")").length;
    } else {
      while (index < matchBody.length && matchBody[index] !== ",") index += 1;
    }
    arms.push(matchBody.slice(patternStart, index).trim());
    if (matchBody[index] === ",") index += 1;
  }
  return arms;
}

function splitMatchArm(arm) {
  let depth = 0;
  let index = 0;
  while (index < arm.length - 1) {
    const skipped = skipRustTrivia(arm, index);
    if (skipped !== null) {
      index = skipped;
      continue;
    }
    if ("([{".includes(arm[index])) depth += 1;
    else if (")]}".includes(arm[index])) depth -= 1;
    else if (depth === 0 && arm.startsWith("=>", index)) {
      return [arm.slice(0, index).trim(), arm.slice(index + 2).trim()];
    }
    index += 1;
  }
  return null;
}

function parseArmString(expression) {
  let value = expression.trim();
  if (value.startsWith("{") && value.endsWith("}")) value = value.slice(1, -1).trim();
  return parseRustString(value);
}

function parseEnglishToolDefaults(source) {
  // English defaults retain only labels; usage-layer seeds and descriptions stay empty.
  const result = new Map();
  const arms = splitMatchArms(
    source,
    "fn english_tool_label(name: &str) -> Option<&'static str>"
  );
  for (const arm of arms) {
    const split = splitMatchArm(arm);
    if (!split) continue;
    const [pattern, expression] = split;
    const name = parseRustString(pattern);
    const label = parseArmString(expression);
    if (name === null || label === null) continue;
    result.set(name, { label, description: "" });
  }
  return result;
}

function parseEnglishParameterLabels(source) {
  const result = new Map();
  const arms = splitMatchArms(
    source,
    "fn english_parameter_label(name: &str) -> Option<&'static str>"
  );
  for (const arm of arms) {
    const split = splitMatchArm(arm);
    if (!split) continue;
    const name = parseRustString(split[0]);
    const value = parseArmString(split[1]);
    if (name !== null && value !== null) result.set(name, value);
  }
  return result;
}

function parseTupleStringMap(source, functionSignature) {
  const result = new Map();
  for (const arm of splitMatchArms(source, functionSignature)) {
    const split = splitMatchArm(arm);
    if (!split) continue;
    const value = parseArmString(split[1]);
    if (value === null) continue;
    const matches = [...split[0].matchAll(/\(\s*("(?:[^"\\]|\\.)*")\s*,\s*("(?:[^"\\]|\\.)*")\s*\)/gu)];
    for (const match of matches) {
      const tool = parseRustString(match[1]);
      const parameter = parseRustString(match[2]);
      result.set(`${tool}\0${parameter}`, value);
    }
  }
  return result;
}

function containsHan(value) {
  return /[\u3400-\u4DBF\u4E00-\u9FFF\uF900-\uFAFF]/u.test(value);
}

function localizeEnglish(tools, source) {
  const toolDefaults = parseEnglishToolDefaults(source);
  const parameterLabels = parseEnglishParameterLabels(source);
  const parameterHelp = parseTupleStringMap(
    source,
    "fn english_parameter_help(tool: &str, parameter: &str) -> Option<&'static str>"
  );
  const parameterPlaceholders = parseTupleStringMap(
    source,
    "fn english_parameter_placeholder(tool: &str, parameter: &str) -> Option<&'static str>"
  );
  return tools.map((tool) => {
    const localized = toolDefaults.get(tool.name);
    assert.ok(localized, `missing English default for ${tool.name}`);
    return {
      ...tool,
      label: localized.label,
      description: localized.description,
      parameters: tool.parameters.map((parameter) => {
        const label = parameterLabels.get(parameter.name);
        assert.ok(label, `missing English label for ${tool.name}.${parameter.name}`);
        const key = `${tool.name}\0${parameter.name}`;
        const next = { ...parameter, label };
        if (parameter.help && containsHan(parameter.help)) {
          assert.ok(parameterHelp.has(key), `missing English help for ${tool.name}.${parameter.name}`);
          next.help = parameterHelp.get(key);
        }
        if (parameter.placeholder && containsHan(parameter.placeholder)) {
          assert.ok(parameterPlaceholders.has(key),
            `missing English placeholder for ${tool.name}.${parameter.name}`);
          next.placeholder = parameterPlaceholders.get(key);
        }
        return next;
      })
    };
  });
}

function render(value) {
  return `${JSON.stringify(value, null, 2)}\n`;
}

async function verifySchemaBaselineCoverage(chineseTools) {
  // The model-visible JSON Schema baseline is maintained by the Rust golden test.
  // JavaScript source parsing would drift because `json!` calls include constant
  // interpolation and helper functions.
  // Regenerate builtin-tool-schemas.json with:
  //   cargo test --lib -- builtin_schemas::tests::regenerate_builtin_schema_baseline --ignored
  // Regular cargo tests keep the baseline current. This script only verifies that
  // the baseline covers every public tool.
  const baselineRaw = await readFile(schemaBaselinePath, "utf8");
  const baseline = JSON.parse(baselineRaw);
  const baselineNames = new Set((baseline.tools ?? []).map((tool) => tool.name));
  for (const tool of chineseTools) {
    assert.ok(baselineNames.has(tool.name),
      `builtin-tool-schemas.json is missing public tool ${tool.name}; regenerate it with the Rust golden test`);
  }
}

function buildOutputs(catalogs) {
  const outputs = new Map();
  let referenceNames = null;
  for (const [language, tools] of catalogs) {
    const names = tools.map((tool) => tool.name);
    if (referenceNames === null) referenceNames = names;
    else assert.deepEqual(names, referenceNames, `${language} tool-name order drifted`);
    const source = "src-tauri/src/catalog.rs::tool_catalog + english_* mappings used by tool_catalog_for_language";
    outputs.set(
      join(outputDirectory, `builtin-tool-catalog.${language}.json`),
      render({
        kind: "mework-builtin-tool-catalog-snapshot",
        language,
        source,
        note: "Authoritative backend design snapshot, not a runtime configuration. Tool and parameter labels/placeholders are retained for adjacent UI redesign; standard function-style provider schemas expose tool descriptions, parameter help/defaults, and specialized schema descriptions/constraints, not UI labels/placeholders.",
        toolCount: tools.length,
        tools
      })
    );
    assert.ok(tools.every((tool) => [...tool.description].length <= 4096),
      `${language} contains a tool description above the runtime 4,096-character limit`);
    const redesignDocument = render({
        name: language === "zh-CN"
          ? "Mework 内置工具上下文重设计基线"
          : "Mework built-in tool context redesign baseline",
        note: "schemaNotes replaces the built-in tool description; usageGuidance is appended after a blank line. Copy this file to .mework/tool-descriptions only when you want Mework to discover it, then explicitly select it in the conversation preset.",
        source,
        toolCount: tools.length,
        tools: tools.map((tool) => ({
          toolName: tool.name,
          schemaNotes: tool.description,
          usageGuidance: ""
        }))
      });
    assert.ok(Buffer.byteLength(redesignDocument, "utf8") < 56 * 1024,
      `${language} redesign file exceeds the runtime 56 KiB write limit`);
    outputs.set(
      join(outputDirectory, `builtin-tool-descriptions.redesign.${language}.json`),
      redesignDocument
    );
  }
  return outputs;
}

async function main() {
  const source = await readFile(catalogPath, "utf8");
  const chinese = parseChineseCatalog(source);
  await verifySchemaBaselineCoverage(chinese);
  const catalogs = new Map([
    ["zh-CN", chinese],
    ["en-US", localizeEnglish(chinese, source)]
  ]);
  const outputs = buildOutputs(catalogs);
  const changed = [];

  for (const [path, expected] of outputs) {
    let current = null;
    try {
      current = await readFile(path, "utf8");
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    if (current === expected) continue;
    changed.push(relative(root, path));
    if (!checkOnly) {
      await mkdir(dirname(path), { recursive: true });
      await writeFile(path, expected, "utf8");
    }
  }

  if (checkOnly && changed.length) {
    throw new Error(`context-injection exports are stale: ${changed.join(", ")}`);
  }
  const verb = checkOnly ? "verified" : "exported";
  console.log(`${verb} ${chinese.length} authoritative public tools in ${catalogs.size} languages into ${outputs.size} design files`);
}

await main();

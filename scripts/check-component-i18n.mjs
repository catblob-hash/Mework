import fs from "node:fs";
import path from "node:path";

const ROOT = process.cwd();

function collectTsx(directory, collected) {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      collectTsx(absolute, collected);
      continue;
    }
    if (!entry.name.endsWith(".tsx") || entry.name.endsWith(".test.tsx")) continue;
    collected.push(path.relative(ROOT, absolute));
  }
  return collected;
}

const COMPONENTS = [
  path.join("src", "App.tsx"),
  ...collectTsx(path.join(ROOT, "src", "components"), []).sort()
];

function skipQuoted(source, start, quote) {
  for (let index = start + 1; index < source.length; index += 1) {
    if (source[index] === "\\") {
      index += 1;
      continue;
    }
    if (source[index] === quote) return index + 1;
  }
  return source.length;
}

function skipLineComment(source, start) {
  const end = source.indexOf("\n", start + 2);
  return end < 0 ? source.length : end + 1;
}

function skipBlockComment(source, start) {
  const end = source.indexOf("*/", start + 2);
  return end < 0 ? source.length : end + 2;
}

// A `/` is a regex literal rather than division when the previous meaningful
// character cannot end an expression. Without this the scanner walks straight
// into the regex body, and a backtick in there (`/(`{3,}|~{3,})/` in
// MarkdownContent.tsx) opens a template literal that swallows the rest of the
// file — every comment after it then reads as literal copy, which is exactly
// how this audit started reporting Chinese *comments* as untranslated UI text.
// `<` is deliberately absent: in TSX a `/` after `<` is a JSX closing tag, not a
// regex. Treating it as one made the scan swallow everything up to the next
// slash, hiding the t(zh-CN, en-US) calls in between. `>` has to stay — it is
// the tail of `=>`, after which a regex really can follow.
const REGEX_MAY_FOLLOW = /[([{,;:=!&|?+\-*/%~^>]/u;
const REGEX_MAY_FOLLOW_KEYWORDS = new Set([
  "return", "typeof", "instanceof", "in", "of", "new", "delete",
  "void", "case", "do", "else", "yield", "await"
]);

function regexAllowedAt(source, index) {
  let cursor = index - 1;
  while (cursor >= 0 && /\s/u.test(source[cursor])) cursor -= 1;
  if (cursor < 0) return true;
  const character = source[cursor];
  if (REGEX_MAY_FOLLOW.test(character)) return true;
  if (!/[\w$]/u.test(character)) return false;
  let start = cursor;
  while (start >= 0 && /[\w$]/u.test(source[start])) start -= 1;
  return REGEX_MAY_FOLLOW_KEYWORDS.has(source.slice(start + 1, cursor + 1));
}

function skipRegex(source, start) {
  let index = start + 1;
  let inClass = false;
  while (index < source.length) {
    const character = source[index];
    if (character === "\\") {
      index += 2;
      continue;
    }
    // A regex literal cannot span lines. Hitting one means the heuristic
    // misfired, so give the single slash back rather than eating a whole block.
    if (character === "\n") return start + 1;
    if (character === "[") inClass = true;
    else if (character === "]") inClass = false;
    else if (character === "/" && !inClass) {
      index += 1;
      while (index < source.length && /[a-z]/u.test(source[index] ?? "")) index += 1;
      return index;
    }
    index += 1;
  }
  return source.length;
}

function collectRanges(source) {
  const translated = [];
  const comments = [];

  // Scans code positions and returns the index just past the region consumed.
  // `stopAtBrace` scans a `${...}` body and returns after its matching `}`.
  function scanCode(start, stopAtBrace) {
    let braces = 0;
    let index = start;
    while (index < source.length) {
      const current = source[index];
      const next = source[index + 1];
      if (current === "'" || current === '"') {
        index = skipQuoted(source, index, current);
        continue;
      }
      if (current === "`") {
        index = scanTemplate(index);
        continue;
      }
      if (current === "/" && next === "/") {
        const end = skipLineComment(source, index);
        comments.push([index, end]);
        index = end;
        continue;
      }
      if (current === "/" && next === "*") {
        const end = skipBlockComment(source, index);
        comments.push([index, end]);
        index = end;
        continue;
      }
      if (current === "/" && regexAllowedAt(source, index)) {
        index = skipRegex(source, index);
        continue;
      }
      if (stopAtBrace && current === "{") braces += 1;
      if (stopAtBrace && current === "}") {
        if (braces === 0) return index + 1;
        braces -= 1;
      }
      const callName = source.startsWith("translate", index)
        ? "translate"
        : current === "t"
          ? "t"
          : "";
      if (
        callName
        && !/[\w$]/u.test(source[index - 1] ?? "")
        && !/[\w$]/u.test(source[index + callName.length] ?? "")
      ) {
        let open = index + callName.length;
        while (/\s/u.test(source[open] ?? "")) open += 1;
        if (source.slice(open, open + 2) === "?.") open += 2;
        while (/\s/u.test(source[open] ?? "")) open += 1;
        if (source[open] === "(") {
          const end = closingParen(open);
          translated.push([index, end]);
          index = end;
          continue;
        }
      }
      index += 1;
    }
    return index;
  }

  // Template chunks hold literal copy, but `${...}` holds real code — and that
  // is where t(zh-CN, en-US) calls inside interpolated strings live.
  function scanTemplate(start) {
    let index = start + 1;
    while (index < source.length) {
      const current = source[index];
      if (current === "\\") {
        index += 2;
        continue;
      }
      if (current === "`") return index + 1;
      if (current === "$" && source[index + 1] === "{") {
        index = scanCode(index + 2, true);
        continue;
      }
      index += 1;
    }
    return source.length;
  }

  function closingParen(open) {
    let depth = 0;
    for (let index = open; index < source.length;) {
      const current = source[index];
      const next = source[index + 1];
      if (current === "'" || current === '"') {
        index = skipQuoted(source, index, current);
        continue;
      }
      if (current === "`") {
        index = scanTemplate(index);
        continue;
      }
      if (current === "/" && next === "/") {
        index = skipLineComment(source, index);
        continue;
      }
      if (current === "/" && next === "*") {
        index = skipBlockComment(source, index);
        continue;
      }
      if (current === "(") depth += 1;
      if (current === ")") {
        depth -= 1;
        if (depth === 0) return index + 1;
      }
      index += 1;
    }
    return source.length;
  }

  scanCode(0, false);
  return { translated, comments };
}

function contained(index, ranges) {
  return ranges.some(([start, end]) => index >= start && index < end);
}

// Discovery and the Han prefilter both run in Node so the audit needs no
// external tools. A small balanced-call scanner then avoids false positives
// when t(zh-CN, en-US) spans multiple lines; TypeScript 7 no longer exposes
// createSourceFile from its stable JS entrypoint.
const failures = [];
const failedLines = new Set();

for (const relative of COMPONENTS) {
  const source = fs.readFileSync(path.join(ROOT, relative), "utf8");
  if (!/\p{Script=Han}/u.test(source)) continue;
  const { translated, comments } = collectRanges(source);
  const lineStarts = [0];
  for (let index = 0; index < source.length; index += 1) {
    if (source[index] === "\n") lineStarts.push(index + 1);
  }
  for (const match of source.matchAll(/\p{Script=Han}/gu)) {
    const index = match.index;
    if (contained(index, translated) || contained(index, comments)) continue;
    let lineIndex = 0;
    while (lineIndex + 1 < lineStarts.length && lineStarts[lineIndex + 1] <= index) lineIndex += 1;
    const lineEnd = source.indexOf("\n", lineStarts[lineIndex]);
    const lineText = source.slice(lineStarts[lineIndex], lineEnd < 0 ? source.length : lineEnd);
    if (lineText.includes("i18n-audit-ignore:")) continue;
    const lineKey = `${relative}:${lineIndex + 1}`;
    if (failedLines.has(lineKey)) continue;
    failedLines.add(lineKey);
    failures.push({
      file: relative,
      line: lineIndex + 1,
      column: index - lineStarts[lineIndex] + 1,
      text: lineText.trim()
    });
  }
}

if (failures.length) {
  console.error("Hard-coded Chinese UI copy must be wrapped in t(zh-CN, en-US) or translate(language, zh-CN, en-US):");
  failures.forEach(({ file, line, column, text }) => console.error(`${file}:${line}:${column}: ${text}`));
  process.exitCode = 1;
} else {
  console.log(`Component i18n audit passed: ${COMPONENTS.length} TSX files.`);
}

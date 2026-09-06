import fs from "node:fs";
import path from "node:path";

const root = process.cwd();
const sourceRoot = path.join(root, "src");
const palettePath = path.join(sourceRoot, "palette.css");
const literalPattern = /#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{4}|[0-9a-fA-F]{3})\b|rgba?\([^)]*\)|(?<![-\w])(?:white|black)(?![-\w])/gi;
const unsupportedColorFunctionPattern = /(?<![-\w])(?:hsl|hsla|hwb|lab|lch|oklab|oklch|color)\s*\(/gi;
const colorReferencePattern = /var\(\s*(--color-[a-z0-9_-]+)/gi;

function listCssFiles(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const fullPath = path.join(directory, entry.name);
    if (entry.isDirectory()) return listCssFiles(fullPath);
    return entry.isFile() && entry.name.endsWith(".css") ? [fullPath] : [];
  });
}

function maskCommentsAndStrings(source) {
  return source.replace(/\/\*[\s\S]*?\*\/|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'/g, (match) =>
    match.replace(/[^\r\n]/g, " ")
  );
}

function lineNumber(source, index) {
  return source.slice(0, index).split(/\r?\n/).length;
}

function parseChannel(value, token) {
  if (value.endsWith("%")) {
    const percent = Number(value.slice(0, -1));
    if (!Number.isFinite(percent) || percent < 0 || percent > 100) {
      throw new Error(`${token} has an invalid RGB percentage channel`);
    }
    return Math.round(percent * 255 / 100);
  }
  const channel = Number(value);
  if (!Number.isInteger(channel) || channel < 0 || channel > 255) {
    throw new Error(`${token} must use integer RGB channels from 0 through 255`);
  }
  return channel;
}

function parseColor(value, token) {
  const normalized = value.trim().toLowerCase();
  if (normalized.startsWith("#")) {
    let hex = normalized.slice(1);
    if (hex.length === 3 || hex.length === 4) {
      hex = [...hex].map((character) => character.repeat(2)).join("");
    }
    if (hex.length !== 6 && hex.length !== 8) {
      throw new Error(`${token} has an unsupported hex color`);
    }
    return {
      red: Number.parseInt(hex.slice(0, 2), 16),
      green: Number.parseInt(hex.slice(2, 4), 16),
      blue: Number.parseInt(hex.slice(4, 6), 16),
      alpha: hex.length === 8 ? `hex:${hex.slice(6, 8)}` : null
    };
  }
  const rgb = normalized.match(/^rgba?\((.*)\)$/);
  if (!rgb) throw new Error(`${token} must be a hex or rgb() color`);
  const body = rgb[1].trim();
  let channels;
  let alpha = null;
  if (body.includes(",")) {
    const parts = body.split(",").map((part) => part.trim());
    if (parts.length !== 3 && parts.length !== 4) {
      throw new Error(`${token} has an unsupported rgb() color`);
    }
    channels = parts.slice(0, 3);
    alpha = parts[3] ?? null;
  } else {
    const [channelSource, alphaSource, ...extra] = body.split("/").map((part) => part.trim());
    if (extra.length) throw new Error(`${token} has an unsupported rgb() color`);
    channels = channelSource.split(/\s+/);
    alpha = alphaSource || null;
  }
  if (channels.length !== 3) throw new Error(`${token} has an unsupported rgb() color`);
  return {
    red: parseChannel(channels[0], token),
    green: parseChannel(channels[1], token),
    blue: parseChannel(channels[2], token),
    alpha: alpha === null ? null : `rgb:${alpha}`
  };
}

function declarations(block, selector) {
  const values = new Map();
  const declarationPattern = /^\s*(--color-[a-z0-9_-]+)\s*:\s*([^;]+);\s*$/gim;
  for (const match of block.matchAll(declarationPattern)) {
    if (values.has(match[1])) throw new Error(`${selector} declares ${match[1]} more than once`);
    values.set(match[1], match[2].trim());
  }
  return values;
}

const cssFiles = listCssFiles(sourceRoot);
const failures = [];
const referencedTokens = new Set();

for (const file of cssFiles) {
  if (file === palettePath) continue;
  const source = fs.readFileSync(file, "utf8");
  const masked = maskCommentsAndStrings(source);
  for (const pattern of [literalPattern, unsupportedColorFunctionPattern]) {
    pattern.lastIndex = 0;
    for (const match of masked.matchAll(pattern)) {
      failures.push(`${path.relative(root, file)}:${lineNumber(masked, match.index)} contains bare color ${match[0]}`);
    }
  }
  colorReferencePattern.lastIndex = 0;
  for (const match of source.matchAll(colorReferencePattern)) referencedTokens.add(match[1]);
}

if (!fs.existsSync(palettePath)) {
  failures.push("src/palette.css is missing");
} else {
  const palette = fs.readFileSync(palettePath, "utf8");
  const blocks = palette.match(/:root\s*\{([\s\S]*?)\}\s*:root\[data-theme="night"\]\s*\{([\s\S]*?)\}/);
  if (!blocks) {
    failures.push('src/palette.css must define :root and :root[data-theme="night"] blocks');
  } else {
    try {
      const day = declarations(blocks[1], ":root");
      const night = declarations(blocks[2], ':root[data-theme="night"]');
      if (!day.size) failures.push("src/palette.css does not define any --color-* tokens");
      for (const [token, dayValue] of day) {
        if (!night.has(token)) {
          failures.push(`Night palette is missing ${token}`);
          continue;
        }
        const dayColor = parseColor(dayValue, token);
        const nightColor = parseColor(night.get(token), token);
        if (
          nightColor.red !== 255 - dayColor.red
          || nightColor.green !== 255 - dayColor.green
          || nightColor.blue !== 255 - dayColor.blue
        ) {
          failures.push(`${token} night RGB is not the exact inverse of its day RGB`);
        }
        if (nightColor.alpha !== dayColor.alpha) {
          failures.push(`${token} changes alpha between day and night`);
        }
      }
      for (const token of night.keys()) {
        if (!day.has(token)) failures.push(`Day palette is missing ${token}`);
      }
      for (const token of referencedTokens) {
        if (!day.has(token)) failures.push(`${token} is referenced but not registered in the palette`);
      }
      for (const token of day.keys()) {
        if (!referencedTokens.has(token)) failures.push(`${token} is registered but unused`);
      }
    } catch (error) {
      failures.push(error instanceof Error ? error.message : String(error));
    }
  }
}

if (failures.length) {
  console.error("Theme color validation failed:");
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(`Theme color validation passed: ${cssFiles.length - 1} CSS files, ${referencedTokens.size} palette tokens.`);

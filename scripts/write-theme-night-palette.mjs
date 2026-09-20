/*
 * Rewrites the night block of `src/palette.css` from its day block.
 *
 * The day block is the authored side of the palette; night is derived, never
 * hand-edited. Add or change a day token, run this, and the night block is
 * brought back in line with `nightFromDay`. `npm run test:theme-colors` checks
 * the same relation, so a night block left stale fails the gate.
 */
import fs from "node:fs";
import path from "node:path";

import { formatColor, nightFromDay, parseColor } from "./theme-night-palette.mjs";

const palettePath = path.join(process.cwd(), "src", "palette.css");
const source = fs.readFileSync(palettePath, "utf8");
const blocks = source.match(/(:root\s*\{)([\s\S]*?)(\}\s*:root\[data-theme="night"\]\s*\{)([\s\S]*?)(\})/);
if (!blocks) throw new Error('src/palette.css must define :root and :root[data-theme="night"] blocks');

const declarationPattern = /^([ \t]*)(--color-[a-z0-9_-]+)([ \t]*:[ \t]*)([^;]+);[ \t]*$/gm;
const night = [];
for (const [, indent, token, separator, value] of blocks[2].matchAll(declarationPattern)) {
  const dayValue = value.trim();
  const nightValue = formatColor(nightFromDay(parseColor(dayValue, token)), dayValue);
  night.push(`${indent}${token}${separator}${nightValue};`);
}
if (!night.length) throw new Error("src/palette.css does not define any --color-* tokens");

const updated = `${blocks[1]}${blocks[2]}${blocks[3]}\n${night.join("\n")}\n${blocks[5]}`;
const rewritten = source.slice(0, blocks.index) + updated + source.slice(blocks.index + blocks[0].length);
if (rewritten === source) {
  console.log(`Night palette already matches the day palette: ${night.length} tokens.`);
} else {
  fs.writeFileSync(palettePath, rewritten);
  console.log(`Rewrote the night palette from the day palette: ${night.length} tokens.`);
}

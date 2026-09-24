/**
 * The metadata block a Markdown document may open with.
 *
 * Static-site generators, Obsidian and skill files all put YAML (`---`) or TOML
 * (`+++`) at the top of a document. Markdown itself knows nothing of it, so left
 * in place it renders as a rule followed by a heading made of its last line.
 */
export interface FrontMatter {
  language: "yaml" | "toml";
  /** The block's contents, without its fences. */
  source: string;
  /** Everything after the block. */
  body: string;
}

const FRONT_MATTER = /^(?:﻿)?(---|\+\+\+)[ \t]*\r?\n([\s\S]*?)\r?\n\1[ \t]*(?:\r?\n|$)/;

export function splitFrontMatter(content: string): FrontMatter | null {
  const match = content.match(FRONT_MATTER);
  if (!match) return null;
  return {
    language: match[1] === "+++" ? "toml" : "yaml",
    source: match[2],
    body: content.slice(match[0].length)
  };
}

/**
 * MathJax itself: TeX in, a lite DOM tree of SVG out.
 *
 * This module is only ever reached through the dynamic import in `./mathJax`, so
 * the engine and its font — a megabyte and a half between them — become one chunk
 * that is fetched the first time a formula is on screen rather than at startup.
 *
 * Output is SVG because it is the one MathJax output that needs nothing from the
 * page: CommonHTML wants a font face and a stylesheet injected at runtime, which the
 * renderer's CSP refuses. The lite adaptor keeps MathJax off the real document —
 * it never measures or inserts anything — and hands back a plain tree the caller
 * turns into React elements.
 */

import { MathJaxMhchemFontExtension } from "@mathjax/mathjax-mhchem-font-extension/js/svg.js";
import { MathJaxNewcmFont } from "@mathjax/mathjax-newcm-font/js/svg.js";
import { liteAdaptor } from "@mathjax/src/js/adaptors/liteAdaptor.js";
import type { LiteElement } from "@mathjax/src/js/adaptors/lite/Element.js";
import type { LiteText } from "@mathjax/src/js/adaptors/lite/Text.js";
import { RegisterHTMLHandler } from "@mathjax/src/js/handlers/html.js";
import { TeX } from "@mathjax/src/js/input/tex.js";
import { mathjax } from "@mathjax/src/js/mathjax.js";
import { SVG } from "@mathjax/src/js/output/svg.js";
import "@mathjax/src/js/input/tex/base/BaseConfiguration.js";
import "@mathjax/src/js/input/tex/ams/AmsConfiguration.js";
import "@mathjax/src/js/input/tex/amscd/AmsCdConfiguration.js";
import "@mathjax/src/js/input/tex/bbox/BboxConfiguration.js";
import "@mathjax/src/js/input/tex/begingroup/BegingroupConfiguration.js";
import "@mathjax/src/js/input/tex/boldsymbol/BoldsymbolConfiguration.js";
import "@mathjax/src/js/input/tex/braket/BraketConfiguration.js";
import "@mathjax/src/js/input/tex/bussproofs/BussproofsConfiguration.js";
import "@mathjax/src/js/input/tex/cancel/CancelConfiguration.js";
import "@mathjax/src/js/input/tex/cases/CasesConfiguration.js";
import "@mathjax/src/js/input/tex/centernot/CenternotConfiguration.js";
import "@mathjax/src/js/input/tex/color/ColorConfiguration.js";
import "@mathjax/src/js/input/tex/colortbl/ColortblConfiguration.js";
import "@mathjax/src/js/input/tex/configmacros/ConfigMacrosConfiguration.js";
import "@mathjax/src/js/input/tex/empheq/EmpheqConfiguration.js";
import "@mathjax/src/js/input/tex/enclose/EncloseConfiguration.js";
import "@mathjax/src/js/input/tex/extpfeil/ExtpfeilConfiguration.js";
import "@mathjax/src/js/input/tex/gensymb/GensymbConfiguration.js";
import "@mathjax/src/js/input/tex/mathtools/MathtoolsConfiguration.js";
import "@mathjax/src/js/input/tex/mhchem/MhchemConfiguration.js";
import "@mathjax/src/js/input/tex/newcommand/NewcommandConfiguration.js";
import "@mathjax/src/js/input/tex/noundefined/NoUndefinedConfiguration.js";
import "@mathjax/src/js/input/tex/physics/PhysicsConfiguration.js";
import "@mathjax/src/js/input/tex/textcomp/TextcompConfiguration.js";
import "@mathjax/src/js/input/tex/textmacros/TextMacrosConfiguration.js";
import "@mathjax/src/js/input/tex/unicode/UnicodeConfiguration.js";
import "@mathjax/src/js/input/tex/units/UnitsConfiguration.js";
import "@mathjax/src/js/input/tex/upgreek/UpgreekConfiguration.js";
import "@mathjax/src/js/input/tex/verb/VerbConfiguration.js";

/**
 * Every TeX package that works without MathJax's own component loader.
 *
 * Left out on purpose: `require` and `autoload` (they load code by name at
 * runtime), `html` and `texhtml` (they put attributes and raw markup of the
 * formula's choosing into the page), `action` (interactive toggles nothing here
 * would wire up), and `noerrors` (a formula that does not parse should say so, not
 * quietly show its source as if it had rendered). `bbm`, `bboldx` and `dsfont`
 * draw from font extensions this bundle does not carry — and `bboldx` redefines
 * `\mathbb` onto its own, so loading it without them breaks the everyday one.
 */
const TEX_PACKAGES = [
  "base", "ams", "amscd", "bbox", "begingroup", "boldsymbol", "braket", "bussproofs", "cancel",
  "cases", "centernot", "color", "colortbl", "configmacros", "empheq", "enclose", "extpfeil",
  "gensymb", "mathtools", "mhchem", "newcommand", "noundefined", "physics", "textcomp",
  "textmacros", "unicode", "units", "upgreek", "verb"
];

// mhchem's arrows are glyphs of their own, shipped as an extension to the font.
MathJaxNewcmFont.addExtension(MathJaxMhchemFontExtension);

/**
 * The font's rarely used ranges, one chunk each.
 *
 * MathJax asks for these by name the first time a formula needs a glyph outside
 * the base set. They are listed as bare specifiers rather than globbed from the
 * package directory so the bundler resolves them the same way it resolves the
 * font itself: each range registers its glyphs on the font class it imports, and
 * a second copy of that class — which a path-based import can produce under the
 * dev server's dependency pre-bundling — would take the glyphs and leave the
 * engine asking forever.
 */
const DYNAMIC_FONT_RANGES: Record<string, () => Promise<unknown>> = {
  PUA: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/PUA.js"),
  "accents-b-i": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/accents-b-i.js"),
  accents: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/accents.js"),
  arabic: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/arabic.js"),
  arrows: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/arrows.js"),
  "braille-d": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/braille-d.js"),
  braille: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/braille.js"),
  calligraphic: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/calligraphic.js"),
  cherokee: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/cherokee.js"),
  "cyrillic-ss": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/cyrillic-ss.js"),
  cyrillic: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/cyrillic.js"),
  devanagari: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/devanagari.js"),
  "double-struck": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/double-struck.js"),
  fraktur: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/fraktur.js"),
  "greek-ss": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/greek-ss.js"),
  greek: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/greek.js"),
  hebrew: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/hebrew.js"),
  "latin-b": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/latin-b.js"),
  "latin-bi": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/latin-bi.js"),
  "latin-i": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/latin-i.js"),
  latin: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/latin.js"),
  marrows: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/marrows.js"),
  math: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/math.js"),
  "monospace-ex": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/monospace-ex.js"),
  "monospace-l": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/monospace-l.js"),
  monospace: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/monospace.js"),
  mshapes: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/mshapes.js"),
  "phonetics-ss": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/phonetics-ss.js"),
  phonetics: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/phonetics.js"),
  "sans-serif-b": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/sans-serif-b.js"),
  "sans-serif-bi": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/sans-serif-bi.js"),
  "sans-serif-ex": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/sans-serif-ex.js"),
  "sans-serif-i": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/sans-serif-i.js"),
  "sans-serif-r": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/sans-serif-r.js"),
  "sans-serif": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/sans-serif.js"),
  script: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/script.js"),
  shapes: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/shapes.js"),
  "symbols-b-i": () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/symbols-b-i.js"),
  symbols: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/symbols.js"),
  variants: () => import("@mathjax/mathjax-newcm-font/js/svg/dynamic/variants.js")
};

const DYNAMIC_PREFIX = "@mathjax/mathjax-newcm-font/js/svg/dynamic/";

/** A node of the tree MathJax answers with, reduced to what a renderer reads. */
export type MathTreeNode =
  | { type: "element"; tag: string; attributes: Record<string, string>; children: MathTreeNode[] }
  | { type: "text"; value: string };

/**
 * One conversion's outcome. `retry` means MathJax needs a font range it has not
 * loaded yet: the promise settles once it has, and converting again will succeed
 * or ask for the next one.
 */
export type MathConversion =
  | { status: "ready"; tree: MathTreeNode }
  | { status: "error"; message: string }
  | { status: "retry"; ready: Promise<void> };

mathjax.asyncLoad = (name: string) => {
  const range = name.startsWith(DYNAMIC_PREFIX)
    ? name.slice(DYNAMIC_PREFIX.length).replace(/\.js$/, "")
    : null;
  const load = range === null ? undefined : DYNAMIC_FONT_RANGES[range];
  return load ? load() : Promise.reject(new Error(`MathJax requested an unknown module: ${name}`));
};

const adaptor = liteAdaptor();
RegisterHTMLHandler(adaptor);

const tex = new TeX({
  packages: TEX_PACKAGES,
  // A parse error is reported to the caller, which shows the source it could not
  // read; MathJax's own `merror` box would render the message in the formula's place.
  formatError: (_jax: unknown, error: unknown) => {
    throw error;
  }
});

const svg = new SVG({
  font: new MathJaxNewcmFont(),
  // Every glyph inline in its own `<path>`: a shared `<defs>` cache would need ids
  // that stay unique across the page, and a cached formula drawn twice would repeat them.
  fontCache: "none",
  // Breaking needs the real line width, which a headless conversion does not have;
  // a long display formula overflows into a box that scrolls sideways instead.
  displayOverflow: "overflow",
  linebreaks: { inline: false }
});

const document = mathjax.document("", { InputJax: tex, OutputJax: svg });

function toTree(node: LiteElement | LiteText): MathTreeNode {
  if (node.kind === "#text") return { type: "text", value: (node as LiteText).value };
  const element = node as LiteElement;
  const attributes: Record<string, string> = {};
  for (const [name, value] of Object.entries(element.attributes)) attributes[name] = String(value);
  return {
    type: "element",
    tag: element.kind,
    attributes,
    children: element.children
      .filter((child) => child.kind !== "#comment")
      .map((child) => toTree(child as LiteElement | LiteText))
  };
}

function isRetry(error: unknown): error is { retry: Promise<unknown> } {
  return typeof error === "object" && error !== null && "retry" in error
    && (error as { retry: unknown }).retry instanceof Promise;
}

/**
 * Converts one formula to SVG, synchronously.
 *
 * The caller owns retries rather than `mathjax.handleRetriesFor`, because the
 * caller is a render: it shows what it has now and asks again when the range
 * lands, instead of holding the whole message back for one glyph.
 */
export function convertTeX(source: string, display: boolean): MathConversion {
  try {
    const node = document.convert(source, { display, em: 16, ex: 8, containerWidth: 1280 });
    // The container is MathJax's `mjx-container`; the renderer draws its own
    // wrapper, so only the drawing inside it is kept.
    const drawing = (node as LiteElement).children.find((child) => child.kind === "svg");
    if (!drawing) return { status: "error", message: "MathJax produced no drawing" };
    return { status: "ready", tree: toTree(drawing as LiteElement) };
  } catch (error) {
    if (isRetry(error)) return { status: "retry", ready: error.retry.then(() => undefined) };
    return { status: "error", message: errorMessage(error) };
  }
}

/** MathJax's `TexError` carries a message but is not an `Error`. */
function errorMessage(error: unknown): string {
  if (typeof error === "object" && error !== null && "message" in error) {
    const { message } = error as { message: unknown };
    if (typeof message === "string" && message) return message;
  }
  return String(error);
}

import { s } from "hastscript";
import type { Element as HastElement, ElementContent } from "hast";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { memo, useSyncExternalStore } from "react";
import type { ReactNode } from "react";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import { mathVersion, peekMath, subscribeMath } from "../lib/mathJax";
import type { MathRender, MathTreeNode } from "../lib/mathJax";

/**
 * The drawing is an `<svg>` and everything in it is SVG, so every element is
 * built in that space: hastscript maps the attribute names (`stroke-width`,
 * `viewBox`) to the properties the JSX runtime expects, which a hand-rolled table
 * would drift from.
 */
function toHast(node: MathTreeNode): ElementContent {
  if (node.type === "text") return { type: "text", value: node.value };
  return s(node.tag, node.attributes, node.children.map(toHast)) as HastElement;
}

/** Each drawing is converted to elements once and shared by every place it appears. */
const drawings = new WeakMap<MathRender, ReactNode>();

function drawingFor(render: Extract<MathRender, { status: "ready" }>): ReactNode {
  const cached = drawings.get(render);
  if (cached !== undefined) return cached;
  const node = toJsxRuntime(toHast(render.tree) as HastElement, {
    Fragment,
    jsx,
    jsxs,
    // `svg` switches the runtime into the SVG namespace for everything inside it.
    space: "svg"
  });
  drawings.set(render, node);
  return node;
}

interface MathFormulaProps {
  /** The TeX between the delimiters, without them. */
  source: string;
  display: boolean;
}

/**
 * One formula, typeset by MathJax.
 *
 * Until MathJax has answered — the first formula on screen waits for the engine to
 * load, an unusual glyph for its font range — the source stands in its place. A
 * formula that will not parse keeps showing its source, with the reason on hover,
 * which is also what a half-streamed `\frac{a}{` looks like until it is finished.
 */
export const MathFormula = memo(function MathFormula({ source, display }: MathFormulaProps) {
  useSyncExternalStore(subscribeMath, mathVersion, mathVersion);
  const render = peekMath(source, display);
  const Wrapper = display ? "div" : "span";
  const delimiter = display ? "$$" : "$";

  if (render === null || render.status === "error") {
    return (
      <Wrapper
        className={`math-formula math-formula--source${display ? " math-formula--display" : ""}`}
        data-math-state={render === null ? "pending" : "error"}
        title={render?.status === "error" ? render.message : undefined}
      >
        <code>{source}</code>
      </Wrapper>
    );
  }

  return (
    <Wrapper
      className={`math-formula${display ? " math-formula--display" : ""}`}
      data-math-state="ready"
      role="math"
      aria-label={source}
    >
      {drawingFor(render)}
      {/* The drawing is not text, so a selection across it would copy nothing. The
          source is carried alongside, out of sight, and a copy picks it up. */}
      <span className="math-formula__source" aria-hidden="true">{`${delimiter}${source}${delimiter}`}</span>
    </Wrapper>
  );
});

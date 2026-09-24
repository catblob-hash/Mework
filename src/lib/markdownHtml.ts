/**
 * The HTML rendered Markdown may carry, and how the rest of it is dropped.
 *
 * Markdown lets raw HTML through, and READMEs and model replies both use it for
 * what Markdown cannot say: `<br>` inside a table cell, `<details>`, `<kbd>`,
 * `<sub>`, a centred logo. It is rendered through GitHub's allow-list — the schema
 * `rehype-sanitize` ships — so it can say exactly what it says on GitHub and no
 * more: no scripts, no event handlers, no styles, no frames, no forms.
 *
 * The schema also has to let through what this app's own Markdown plugins put in
 * the tree before the sanitizer sees it: math, whose class says which kind it is,
 * and the file-path buttons `./remarkPathLinks` makes.
 */

import { defaultSchema } from "rehype-sanitize";
import type { Options as SanitizeSchema } from "rehype-sanitize";

const defaultAttributes = defaultSchema.attributes ?? {};

export const MARKDOWN_HTML_SCHEMA: SanitizeSchema = {
  ...defaultSchema,
  tagNames: [
    ...(defaultSchema.tagNames ?? []),
    "abbr", "button", "caption", "cite", "col", "colgroup", "dfn", "figcaption", "figure", "mark",
    "small", "time", "u", "wbr"
  ],
  attributes: {
    ...defaultAttributes,
    code: [["className", /^language-./, "math-inline", "math-display"]],
    button: [["className", "md-path-link", "md-path-link--code"], ["type", "button"], "title", "data*"],
    img: [...(defaultAttributes.img ?? []), "alt", "title", "width", "height", "align"]
  },
  protocols: {
    ...defaultSchema.protocols,
    // A local file link is handed to the file pane or the file manager, never
    // navigated; a data URL in an image stays an image.
    href: [...(defaultSchema.protocols?.href ?? []), "file"],
    src: [...(defaultSchema.protocols?.src ?? []), "data"]
  }
};

/**
 * Whether `content` has anything that could be an HTML tag.
 *
 * The raw-HTML pass re-parses the whole tree, which a streamed reply would pay
 * for on every token; most replies have no HTML at all, and this is the test
 * that lets them skip it. A false positive only costs the pass.
 */
export function mayContainHtml(content: string): boolean {
  return /<(?:[A-Za-z][\w-]*[\s/>]|\/[A-Za-z]|!--)/.test(content);
}

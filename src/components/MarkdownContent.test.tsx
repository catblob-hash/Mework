import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MarkdownContent, normalizeMathDelimiters } from "./MarkdownContent";

describe("MarkdownContent", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("renders common Markdown without executing embedded HTML", () => {
    const { container } = render(
      <MarkdownContent content={`# 标题

**重点**与~~删除~~

- [x] 已完成

| 项目 | 值 |
| --- | ---: |
| A | 1 |

\`行内代码\`

\`\`\`ts
const answer = 42;
\`\`\`

[外部链接](https://example.com)

<script>window.__unsafe = true</script>`} />
    );

    expect(screen.getByRole("heading", { name: "标题" })).toBeInTheDocument();
    expect(screen.getByText("重点").tagName).toBe("STRONG");
    expect(screen.getByText("删除").tagName).toBe("DEL");
    expect(screen.getByRole("checkbox")).toBeChecked();
    expect(within(screen.getByRole("table")).getByText("A")).toBeInTheDocument();
    const block = container.querySelector("pre > code.language-ts");
    expect(block).toHaveTextContent("const answer = 42;");
    // Coloured by the grammar the fence names, and copyable from its corner.
    expect(block?.querySelector(".code-token--keyword")).toHaveTextContent("const");
    expect(screen.getByRole("button", { name: /Copy code|复制代码/ })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "外部链接" })).toHaveAttribute("target", "_blank");
    expect(container.querySelector("script")).not.toBeInTheDocument();
  });

  it("renders inline and display formulas with dollar and LaTeX delimiters", async () => {
    const { container } = render(
      <MarkdownContent content={`行内 $E=mc^2$ 与 \\(a^2+b^2=c^2\\)。

$$\\int_0^1 x^2\\,dx=\\frac{1}{3}$$

\\[\\sum_{i=1}^{n} i=\\frac{n(n+1)}{2}\\]`} />
    );

    await waitFor(() => expect(container.querySelectorAll(".math-formula[data-math-state='ready']")).toHaveLength(4));
    expect(container.querySelectorAll(".math-formula--display")).toHaveLength(2);
    expect(container.querySelectorAll(".math-formula svg")).toHaveLength(4);
    // The drawing is announced by its source, and carries it for a copy.
    expect(screen.getByRole("math", { name: "E=mc^2" })).toBeInTheDocument();
    expect(container).toHaveTextContent("$E=mc^2$");
    expect(container).toHaveTextContent("$$\\sum_{i=1}^{n} i=\\frac{n(n+1)}{2}$$");
  });

  it("renders a fence that says math as display math", async () => {
    const { container } = render(<MarkdownContent content={"```math\n\\frac{a}{b}\n```"} />);
    await waitFor(() => expect(container.querySelector(".math-formula--display[data-math-state='ready']")).toBeInTheDocument());
    expect(container.querySelector("pre")).not.toBeInTheDocument();
  });

  it("shows a formula that does not parse as its source, with the reason", async () => {
    const { container } = render(<MarkdownContent content={"未完 $\\frac{a}{$ 与 $x$"} />);
    await waitFor(() => expect(container.querySelector(".math-formula[data-math-state='error']")).toBeInTheDocument());
    const broken = container.querySelector(".math-formula[data-math-state='error']");
    expect(broken).toHaveTextContent("\\frac{a}{");
    expect(broken?.getAttribute("title")).toBeTruthy();
    await waitFor(() => expect(container.querySelector(".math-formula[data-math-state='ready']")).toBeInTheDocument());
  });

  it("does not rewrite math-like delimiters inside code", () => {
    const source = "文本 \\(x\\) `\\(inline\\)`\n\n```txt\n\\[block\\]\n```\n之后 \\[y\\]";
    expect(normalizeMathDelimiters(source)).toBe("文本 $x$ `\\(inline\\)`\n\n```txt\n\\[block\\]\n```\n之后 \n$$\ny\n$$\n");
  });

  it("renders Markdown and math throughout an incrementally updated stream", async () => {
    const content = "## 流式标题\n\n$E=mc^2$";
    const { container, rerender } = render(<MarkdownContent content={content} streaming />);

    expect(screen.getByRole("heading", { name: "流式标题" })).toBeInTheDocument();
    await waitFor(() => expect(container.querySelector(".math-formula[data-math-state='ready']")).toBeInTheDocument());

    rerender(<MarkdownContent content={`${content}\n\n- 第一项\n- 第二项`} streaming />);
    expect(screen.getByRole("list")).toBeInTheDocument();
    expect(screen.getByText("第二项")).toBeInTheDocument();
    // A formula already typeset is drawn again from the cache, not re-typeset.
    expect(container.querySelector(".math-formula[data-math-state='ready']")).toBeInTheDocument();

    rerender(<MarkdownContent content={`${content}\n\n- 第一项\n- 第二项`} />);
    expect(screen.getByRole("heading", { name: "流式标题" })).toBeInTheDocument();
    expect(container.querySelector(".math-formula[data-math-state='ready']")).toBeInTheDocument();
  });

  it("sweeps only the newly arrived text, and stops marking settled text", () => {
    const fresh = (container: HTMLElement) => Array.from(container.querySelectorAll<HTMLElement>(".stream-fresh"));
    const { container, rerender } = render(<MarkdownContent content="前半段" streaming />);
    // Nothing was on screen yet, so all of it is new.
    expect(fresh(container).map((span) => span.textContent)).toEqual(["前半段"]);
    const firstTick = fresh(container)[0].className;

    // Only the continuation moves, and under the other tick: React keeps the
    // span, and an unchanged animation name would not replay.
    rerender(<MarkdownContent content="前半段，加上后半段" streaming />);
    expect(fresh(container).map((span) => span.textContent)).toEqual(["，加上后半段"]);
    expect(fresh(container)[0].className).not.toBe(firstTick);
    expect(container.querySelector("p")).toHaveTextContent("前半段，加上后半段");

    // The same text again is not new text: the sweep in flight is left alone
    // rather than restarted, which would re-fade what the reader already has.
    const settledTick = fresh(container)[0].className;
    rerender(<MarkdownContent content="前半段，加上后半段" streaming />);
    expect(fresh(container).map((span) => span.textContent)).toEqual(["，加上后半段"]);
    expect(fresh(container)[0].className).toBe(settledTick);

    // Settled text carries no reveal at all.
    rerender(<MarkdownContent content="前半段，加上后半段" />);
    expect(fresh(container)).toHaveLength(0);
  });

  it("sweeps text by what renders, across blocks in reading order, and leaves code alone", () => {
    const fresh = (container: HTMLElement) => Array.from(container.querySelectorAll<HTMLElement>(".stream-fresh"));
    const { container, rerender } = render(<MarkdownContent content="先看 **bo" streaming />);

    // `**bo` was literal text; closed, it renders as bold `bo` plus the rest,
    // and it is the rendered text that is new.
    rerender(<MarkdownContent content={"先看 **bold** 然后\n\n第二段\n\n```ts\nconst x = 1;\n```"} streaming />);
    const spans = fresh(container);
    expect(spans.map((span) => span.textContent)).toEqual(["bold", " 然后", "第二段"]);
    expect(spans[0].closest("strong")).not.toBeNull();
    // One stroke in reading order: each span starts where the text before it ends.
    const delays = spans.map((span) => Number.parseFloat(span.style.animationDelay));
    expect(delays[0]).toBe(0);
    expect(delays[1]).toBeGreaterThan(delays[0]);
    expect(delays[2]).toBeGreaterThan(delays[1]);
    expect(delays[2] + Number.parseFloat(spans[2].style.animationDuration)).toBeLessThanOrEqual(100);
    // A code block is drawn from its tree's text, which a span would break.
    expect(container.querySelector("pre .stream-fresh")).toBeNull();
    expect(container.querySelector("pre")).toHaveTextContent("const x = 1;");
  });

  it("defers historical Markdown outside the viewport and releases it again after scrolling away", () => {
    let callback: IntersectionObserverCallback | null = null;
    let observed: Element | null = null;
    class IntersectionObserverMock {
      readonly root = null;
      readonly rootMargin = "1200px 0px";
      readonly thresholds = [0];
      constructor(next: IntersectionObserverCallback) { callback = next; }
      observe(target: Element) { observed = target; }
      unobserve() { /* no-op */ }
      disconnect() { /* no-op */ }
      takeRecords(): IntersectionObserverEntry[] { return []; }
    }
    vi.stubGlobal("IntersectionObserver", IntersectionObserverMock);
    const { container } = render(<MarkdownContent content="# 屏外标题" deferOffscreen />);
    const host = container.querySelector<HTMLElement>(".markdown-content")!;

    expect(host).toHaveAttribute("data-markdown-deferred", "true");
    expect(screen.queryByRole("heading", { name: "屏外标题" })).not.toBeInTheDocument();

    act(() => callback?.([{ target: observed!, isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver));
    expect(screen.getByRole("heading", { name: "屏外标题" })).toBeInTheDocument();

    act(() => callback?.([{ target: observed!, isIntersecting: false } as IntersectionObserverEntry], {} as IntersectionObserver));
    expect(screen.queryByRole("heading", { name: "屏外标题" })).not.toBeInTheDocument();
    expect(host).toHaveAttribute("data-markdown-deferred", "true");
  });
});

describe("MarkdownContent path links", () => {
  const sample = "见 `src/App.tsx:12` 与 C:\\Windows\\notepad.exe，注意 and/or 与 https://example.com/a/b";
  const baseDir = "C:\\work";

  function targets(container: HTMLElement): string[] {
    return [...container.querySelectorAll<HTMLElement>("[data-mework-path]")].map(
      (node) => node.getAttribute("data-mework-path") ?? ""
    );
  }

  it("leaves the content untouched unless the caller opts in", () => {
    const { container } = render(<MarkdownContent content={sample} />);
    expect(targets(container)).toEqual([]);
    expect(container.querySelector(".markdown-content")).not.toHaveAttribute("data-mework-path-base");
  });

  it("links only what qualifies as a path", () => {
    const { container } = render(<MarkdownContent content={sample} linkifyPaths pathBaseDir={baseDir} />);
    expect(targets(container)).toEqual(["src/App.tsx", "C:\\Windows\\notepad.exe"]);
    // The line reference stays visible even though it is not sent to the host.
    expect(container.querySelector("[data-mework-path]")).toHaveTextContent("src/App.tsx:12");
    // The address remains an ordinary anchor for the external-link interceptor.
    const link = container.querySelector("a");
    expect(link).toHaveAttribute("href", "https://example.com/a/b");
    expect(link).not.toHaveAttribute("data-mework-path");
    expect(container.textContent).toContain("and/or");
  });

  it("publishes the base directory for the click interceptor to read", () => {
    const { container } = render(<MarkdownContent content={sample} linkifyPaths pathBaseDir={baseDir} />);
    expect(container.querySelector(".markdown-content")).toHaveAttribute("data-mework-path-base", baseDir);
  });

  it("keeps paths inside fenced code blocks as plain code", () => {
    const { container } = render(
      <MarkdownContent content={"```\nsrc/App.tsx\n```"} linkifyPaths pathBaseDir={baseDir} />
    );
    expect(targets(container)).toEqual([]);
  });
});

describe("MarkdownContent HTML", () => {
  const html = `| a | b |
| --- | --- |
| 一<br>二 | <kbd>Ctrl</kbd> |

<details><summary>更多</summary>

内容 H<sub>2</sub>O

</details>

<p align="center" style="color: red" onclick="alert(1)">居中</p>

<script>window.__unsafe = true</script><iframe src="https://example.com"></iframe>`;

  it("shows HTML as the text it is unless the surface opts in", () => {
    const { container } = render(<MarkdownContent content={html} />);
    expect(container.querySelector("details")).not.toBeInTheDocument();
    expect(container).toHaveTextContent("<kbd>Ctrl</kbd>");
  });

  it("renders the allowed HTML and drops everything that could run or restyle", () => {
    const { container } = render(<MarkdownContent content={html} renderHtml />);
    expect(container.querySelector("td br")).toBeInTheDocument();
    expect(container.querySelector("kbd")).toHaveTextContent("Ctrl");
    expect(container.querySelector("details summary")).toHaveTextContent("更多");
    expect(container.querySelector("sub")).toHaveTextContent("2");
    const centred = screen.getByText("居中");
    expect(centred).toHaveAttribute("align", "center");
    expect(centred).not.toHaveAttribute("style");
    expect(centred).not.toHaveAttribute("onclick");
    expect(container.querySelector("script, iframe")).not.toBeInTheDocument();
  });

  it("keeps math and detected paths working alongside HTML", async () => {
    const { container } = render(
      <MarkdownContent content={"见 `src/a.ts:3`<br>和 $x^2$"} renderHtml linkifyPaths pathBaseDir="/w" />
    );
    expect(container.querySelector("[data-mework-path='src/a.ts']")).toHaveAttribute("data-mework-path-line", "3");
    await waitFor(() => expect(container.querySelector(".math-formula[data-math-state='ready']")).toBeInTheDocument());
  });
});

describe("MarkdownContent links", () => {
  it("turns a link to a file into a path the file pane can open, line included", () => {
    const { container } = render(
      <MarkdownContent content={"打开 [App](src/App.tsx#L12) 或 [配置](file:///w/Cargo.toml:3)"} linkifyPaths pathBaseDir="/w" />
    );
    const links = [...container.querySelectorAll("a[data-mework-path]")];
    expect(links.map((link) => [link.getAttribute("data-mework-path"), link.getAttribute("data-mework-path-line")])).toEqual([
      ["src/App.tsx", "12"],
      ["/w/Cargo.toml", "3"]
    ]);
    // A click nobody claims must not navigate the app to a relative address.
    const click = new MouseEvent("click", { bubbles: true, cancelable: true });
    links[0].dispatchEvent(click);
    expect(click.defaultPrevented).toBe(true);
  });

  it("leaves document links for the caller to resolve against the document", () => {
    const { container } = render(
      <MarkdownContent content={"[下一篇](other.md)"} linkifyPaths pathBaseDir="/w" documentLinks />
    );
    expect(container.querySelector("a")).toHaveAttribute("href", "other.md");
    expect(container.querySelector("a")).not.toHaveAttribute("data-mework-path");
  });

  it("scrolls to the heading a fragment names, inside its own reply", () => {
    const scrolled: string[] = [];
    const original = Element.prototype.scrollIntoView;
    Element.prototype.scrollIntoView = function scrollIntoView(this: Element) {
      scrolled.push(this.textContent ?? "");
    };
    try {
      render(<MarkdownContent content={"[跳到安装](#安装-步骤)\n\n## 安装 步骤\n\n正文"} />);
      fireEvent.click(screen.getByRole("link", { name: "跳到安装" }));
      expect(scrolled).toEqual(["安装 步骤"]);
    } finally {
      Element.prototype.scrollIntoView = original;
    }
  });
});

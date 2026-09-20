import { act, render, screen, within } from "@testing-library/react";
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
    expect(screen.getByText("const answer = 42;")).toHaveClass("language-ts");
    expect(screen.getByRole("link", { name: "外部链接" })).toHaveAttribute("target", "_blank");
    expect(container.querySelector("script")).not.toBeInTheDocument();
  });

  it("renders inline and display formulas with dollar and LaTeX delimiters", () => {
    const { container } = render(
      <MarkdownContent content={`行内 $E=mc^2$ 与 \\(a^2+b^2=c^2\\)。

$$\\int_0^1 x^2\\,dx=\\frac{1}{3}$$

\\[\\sum_{i=1}^{n} i=\\frac{n(n+1)}{2}\\]`} />
    );

    expect(container.querySelectorAll(".katex")).toHaveLength(4);
    expect(container.querySelectorAll(".katex-display")).toHaveLength(2);
    expect(container).toHaveTextContent("E=mc2");
    expect(container).toHaveTextContent("∫");
    expect(container).toHaveTextContent("∑");
  });

  it("does not rewrite math-like delimiters inside code", () => {
    const source = "文本 \\(x\\) `\\(inline\\)`\n\n```txt\n\\[block\\]\n```\n之后 \\[y\\]";
    expect(normalizeMathDelimiters(source)).toBe("文本 $x$ `\\(inline\\)`\n\n```txt\n\\[block\\]\n```\n之后 \n$$\ny\n$$\n");
  });

  it("renders Markdown and math throughout an incrementally updated stream", () => {
    const content = "## 流式标题\n\n$E=mc^2$";
    const { container, rerender } = render(<MarkdownContent content={content} streaming />);

    expect(screen.getByRole("heading", { name: "流式标题" })).toBeInTheDocument();
    expect(container.querySelector(".katex")).toBeInTheDocument();

    rerender(<MarkdownContent content={`${content}\n\n- 第一项\n- 第二项`} streaming />);
    expect(screen.getByRole("list")).toBeInTheDocument();
    expect(screen.getByText("第二项")).toBeInTheDocument();

    rerender(<MarkdownContent content={`${content}\n\n- 第一项\n- 第二项`} />);
    expect(screen.getByRole("heading", { name: "流式标题" })).toBeInTheDocument();
    expect(container.querySelector(".katex")).toBeInTheDocument();
  });

  it("sweeps only the newly arrived tail, and stops marking settled text", () => {
    // The reveal must restart each commit (an unchanged animation name would
    // not replay) and must start where the previous commit ended, so settled
    // text is not re-faded every 100 ms.
    const { container, rerender } = render(<MarkdownContent content="前半段" streaming />);
    const host = container.querySelector<HTMLElement>(".markdown-content")!;
    const firstTick = host.getAttribute("data-stream-tick");
    expect(firstTick).not.toBeNull();

    rerender(<MarkdownContent content="前半段，加上后半段" streaming />);
    expect(host.getAttribute("data-stream-tick")).not.toBe(firstTick);
    const from = host.style.getPropertyValue("--stream-reveal-from");
    expect(Number.parseInt(from, 10)).toBeGreaterThan(0);
    expect(Number.parseInt(from, 10)).toBeLessThan(100);

    // Same content again: nothing new arrived, so the tick must not advance —
    // advancing would restart the sweep and re-fade text the reader already
    // has, which is what makes a fixed-cadence stream look like it is blinking.
    const settledTick = host.getAttribute("data-stream-tick");
    rerender(<MarkdownContent content="前半段，加上后半段" streaming />);
    expect(host.getAttribute("data-stream-tick")).toBe(settledTick);

    // Settled text carries no reveal state at all.
    rerender(<MarkdownContent content="前半段，加上后半段" />);
    expect(host).not.toHaveAttribute("data-stream-tick");
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

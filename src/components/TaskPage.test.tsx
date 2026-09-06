import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TaskPage } from "./TaskPage";

let observed: Element[] = [];

beforeEach(() => {
  observed = [];
  vi.stubGlobal("ResizeObserver", class ResizeObserverMock {
    observe(target: Element) {
      observed.push(target);
    }
    unobserve() {}
    disconnect() {}
  });
});

afterEach(() => vi.unstubAllGlobals());

describe("TaskPage", () => {
  it("shows the kind and the title, and offers a way back", async () => {
    const user = userEvent.setup();
    const onBack = vi.fn();
    render(
      <TaskPage active eyebrow="预览" title="example.test" onBack={onBack}>
        <p>页面内容</p>
      </TaskPage>
    );

    expect(screen.getByRole("heading", { name: "example.test" })).toBeInTheDocument();
    expect(screen.getByText("预览")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "返回对话" }));
    expect(onBack).toHaveBeenCalledTimes(1);
  });

  /** Pages stay mounted so a preview keeps its address bar and a review keeps its loaded diff. */
  it("hides an inactive page without unmounting it", () => {
    const { container } = render(
      <TaskPage active={false} eyebrow="审阅" title="变更" onBack={() => undefined}>
        <p>差异</p>
      </TaskPage>
    );

    const page = container.querySelector(".task-page");
    expect(page).toHaveAttribute("hidden");
    // Still in the tree: reading textContent proves it was never unmounted, which innerText on a
    // hidden subtree could not.
    expect(page?.textContent).toContain("差异");
  });

  /**
   * The approval and question docks live inside the conversation pane, which is hidden while a
   * page is up. Without this banner a model blocked on approval would simply look stuck.
   */
  it("surfaces a pending conversation prompt and routes back to it", async () => {
    const user = userEvent.setup();
    const onGoBack = vi.fn();
    render(
      <TaskPage
        active
        eyebrow="预览"
        title="example.test"
        attention={{ label: "模型在等待工具批准", onGoBack }}
        onBack={() => undefined}
      >
        <p>页面内容</p>
      </TaskPage>
    );

    await user.click(screen.getByRole("button", { name: /模型在等待工具批准/ }));
    expect(onGoBack).toHaveBeenCalledTimes(1);
  });

  it("says nothing when the conversation needs nothing", () => {
    render(
      <TaskPage active eyebrow="预览" title="example.test" onBack={() => undefined}>
        <p>页面内容</p>
      </TaskPage>
    );

    expect(screen.queryByRole("button", { name: /等待/ })).not.toBeInTheDocument();
  });

  /**
   * The native browser page is a child window above the renderer, positioned from this rectangle.
   * It has to be the content box alone: the host reserves `occludedTop` inside it for the
   * browser's own chrome, so measuring the header too would slide the page under it.
   */
  it("publishes the content rectangle — not the whole page — while active", () => {
    const onContentBoundsChange = vi.fn();
    const { container } = render(
      <TaskPage
        active
        eyebrow="预览"
        title="example.test"
        onBack={() => undefined}
        onContentBoundsChange={onContentBoundsChange}
      >
        <p>页面内容</p>
      </TaskPage>
    );

    expect(onContentBoundsChange).toHaveBeenCalledTimes(1);
    expect(onContentBoundsChange.mock.calls[0]![0]).toMatchObject({
      x: expect.any(Number),
      y: expect.any(Number),
      width: expect.any(Number),
      height: expect.any(Number)
    });
    expect(observed).toEqual([container.querySelector(".task-page__content")]);
  });

  /** An inactive page's geometry is meaningless — a hidden box measures zero. */
  it("publishes nothing while inactive", () => {
    const onContentBoundsChange = vi.fn();
    render(
      <TaskPage
        active={false}
        eyebrow="预览"
        title="example.test"
        onBack={() => undefined}
        onContentBoundsChange={onContentBoundsChange}
      >
        <p>页面内容</p>
      </TaskPage>
    );

    expect(onContentBoundsChange).not.toHaveBeenCalled();
    expect(observed).toEqual([]);
  });
});

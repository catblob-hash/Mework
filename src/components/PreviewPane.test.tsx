import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import type { PreviewConfiguredServer, PreviewServerSnapshot } from "../lib/preview";
import { PreviewServerMenuItems, PreviewStartPage, previewRowDetail, previewServerRows } from "./PreviewPane";

function configuredServer(name: string, port: number): PreviewConfiguredServer {
  return { name, command: "npm", args: ["run", "dev"], cwd: "C:\\work\\mework", port };
}

/** The attach form: a url and no command, exactly as the launch.json format notes describe it. */
function attachServer(name: string, url: string, port = 0): PreviewConfiguredServer {
  return { name, command: null, args: [], cwd: "C:\\work\\mework", port, url };
}

function runningServer(name: string, port: number): PreviewServerSnapshot {
  return {
    handle: `srv-${name}`,
    serverId: name,
    name,
    port,
    status: "running",
    startedAt: "2026-09-09T00:00:00Z",
    cwd: "C:\\work\\mework",
    sessionId: null
  };
}

describe("preview start page", () => {
  beforeEach(() => configureI18n("en-US"));

  afterEach(() => {
    cleanup();
    configureI18n("zh-CN");
  });

  it("marks the entries that attach instead of running", () => {
    const rows = previewServerRows(
      [
        configuredServer("web", 5173),
        attachServer("docs", "https://example.com/docs"),
        attachServer("api", "http://localhost:8443", 8443)
      ],
      [runningServer("web", 5173)]
    );

    expect(rows.map((row) => [row.name, row.attach])).toEqual([
      ["web", false],
      ["docs", true],
      ["api", true]
    ]);
    // A process is addressed by port; an attach row by the url, which for a non-localhost
    // server has no port to print at all.
    expect(rows.map(previewRowDetail)).toEqual([":5173", "example.com", "localhost:8443"]);
  });

  it("opens an attach row without running anything, and offers no run that could only fail", async () => {
    const user = userEvent.setup();
    const rows = previewServerRows([attachServer("docs", "https://example.com/docs")], []);
    const onRun = vi.fn();
    const onOpen = vi.fn();

    render(
      <PreviewStartPage
        rows={rows}
        pendingName={null}
        onRun={onRun}
        onStop={vi.fn()}
        onOpen={onOpen}
      />
    );

    const open = screen.getByRole("button", { name: "Open docs" });
    expect(open).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Run docs" })).not.toBeInTheDocument();

    await user.click(open);

    expect(onOpen).toHaveBeenCalledWith(rows[0]);
    expect(onRun).not.toHaveBeenCalled();
  });

  it("still gates a runnable configuration's open button on the process answering it", () => {
    const rows = previewServerRows([configuredServer("web", 5173)], []);

    render(
      <PreviewStartPage
        rows={rows}
        pendingName={null}
        onRun={vi.fn()}
        onStop={vi.fn()}
        onOpen={vi.fn()}
      />
    );

    expect(screen.getByRole("button", { name: "Open web" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Run web" })).toBeEnabled();
  });
});

describe("preview server menu items", () => {
  beforeEach(() => configureI18n("en-US"));

  afterEach(() => {
    cleanup();
    configureI18n("zh-CN");
  });

  /** A configuration nothing answers: the row is the run, and `Run` is only a label on it. */
  it("makes the whole row the run affordance and leaves Run as a label", async () => {
    const user = userEvent.setup();
    const rows = previewServerRows([configuredServer("web", 5173)], []);
    const onRun = vi.fn();

    render(
      <PreviewServerMenuItems
        rows={rows}
        pendingName={null}
        onOpen={vi.fn()}
        onRun={onRun}
        onStop={vi.fn()}
        onStopAll={vi.fn()}
      />
    );

    expect(screen.queryByRole("button", { name: "Stop web" })).not.toBeInTheDocument();
    const tag = document.querySelector(".browser-panel__menu-server-tag");
    expect(tag).toHaveTextContent("Run");
    expect(tag?.closest("button")).toHaveAttribute("role", "menuitemradio");

    await user.click(screen.getByRole("menuitemradio", { name: "Run web" }));

    expect(onRun).toHaveBeenCalledWith("web");
  });

  /** Once a process answers, the label becomes a real Stop control and the row opens the page. */
  it("turns the label into a stop control only while the server runs", async () => {
    const user = userEvent.setup();
    const rows = previewServerRows([configuredServer("web", 5173)], [runningServer("web", 5173)]);
    const onOpen = vi.fn();
    const onStop = vi.fn();

    render(
      <PreviewServerMenuItems
        rows={rows}
        pendingName={null}
        onOpen={onOpen}
        onRun={vi.fn()}
        onStop={onStop}
        onStopAll={vi.fn()}
      />
    );

    expect(document.querySelector(".browser-panel__menu-server-tag")).not.toBeInTheDocument();
    const stop = screen.getByRole("button", { name: "Stop web" });
    expect(stop).toHaveClass("is-stop");

    await user.click(screen.getByRole("menuitemradio", { name: "Open web" }));
    expect(onOpen).toHaveBeenCalledWith(rows[0]);

    await user.click(stop);
    expect(onStop).toHaveBeenCalledWith(rows[0]);
  });
});

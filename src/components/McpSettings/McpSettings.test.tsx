import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../../i18n";
import type { McpProbeReport, McpServerConfig } from "../../types";

const runtimeMocks = vi.hoisted(() => ({
  probeMcpServer: vi.fn(),
  cancelMcpProbe: vi.fn()
}));

vi.mock("../../lib/backend", () => ({ hasBackendRuntime: () => true }));
vi.mock("../../lib/runtime", () => ({
  probeMcpServer: runtimeMocks.probeMcpServer,
  cancelMcpProbe: runtimeMocks.cancelMcpProbe
}));

import { McpSettings } from ".";

function server(overrides: Partial<McpServerConfig> = {}): McpServerConfig {
  return {
    id: "mcp-one",
    name: "Alpha",
    description: "First server",
    enabled: true,
    transport: "stdio",
    command: "node",
    args: [],
    env: {},
    registryUrl: "",
    url: "",
    headers: {},
    timeoutSeconds: 0,
    longRunning: false,
    provider: "",
    providerUrl: "",
    tags: [],
    disabledTools: [],
    disabledAutoApproveTools: [],
    sortOrder: 0,
    createdAt: "2026-08-22T00:00:00.000Z",
    updatedAt: "2026-08-22T00:00:00.000Z",
    ...overrides
  };
}

function report(overrides: Partial<McpProbeReport> = {}): McpProbeReport {
  return {
    ok: true,
    cancelled: false,
    protocolVersion: "2025-06-18",
    serverName: "alpha-server",
    serverVersion: "1.4.0",
    tools: [],
    prompts: [],
    resources: [],
    logs: [],
    error: "",
    ...overrides
  };
}

function renderSettings(initial: McpServerConfig[]) {
  let current = initial;
  const changes: McpServerConfig[][] = [];
  function Harness() {
    const [servers, setServers] = useState(initial);
    current = servers;
    return (
      <McpSettings
        servers={servers}
        onChange={(next) => {
          changes.push(next);
          setServers(next);
        }}
      />
    );
  }
  return { ...render(<Harness />), getServers: () => current, changes };
}

async function openImport(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Open add menu" }));
  await user.click(screen.getByRole("menuitem", { name: "Import from JSON" }));
}

async function openDetail(user: ReturnType<typeof userEvent.setup>, name: string) {
  await user.click(screen.getByRole("button", { name }));
}

afterEach(() => configureI18n("zh-CN"));

describe("McpSettings", () => {
  beforeEach(() => {
    configureI18n("en-US");
    runtimeMocks.probeMcpServer.mockReset();
    runtimeMocks.cancelMcpProbe.mockReset();
    runtimeMocks.cancelMcpProbe.mockResolvedValue(undefined);
  });

  it("reports an imported server name that already exists", async () => {
    const user = userEvent.setup();
    const { getServers } = renderSettings([server()]);
    await openImport(user);
    fireEvent.change(screen.getByLabelText("MCP server JSON configuration"), {
      target: { value: JSON.stringify({ mcpServers: { Alpha: { command: "node" } } }) }
    });
    await user.click(screen.getByRole("button", { name: "Confirm" }));

    const alert = screen.getByRole("alert");
    expect(within(alert).getByText("Alpha")).toBeInTheDocument();
    expect(within(alert).getByText("Name already exists")).toBeInTheDocument();
    expect(getServers()).toHaveLength(1);
  });

  it("reports duplicate names inside one JSON payload", async () => {
    const user = userEvent.setup();
    const { getServers } = renderSettings([]);
    await openImport(user);
    fireEvent.change(screen.getByLabelText("MCP server JSON configuration"), {
      target: { value: "{\"mcpServers\":{\"Twin\":{\"command\":\"one\"},\"Twin\":{\"command\":\"two\"}}}" }
    });
    await user.click(screen.getByRole("button", { name: "Confirm" }));

    const alert = screen.getByRole("alert");
    expect(within(alert).getByText("Twin")).toBeInTheDocument();
    expect(within(alert).getByText("Duplicate name within this import")).toBeInTheDocument();
    expect(getServers()).toHaveLength(0);
  });

  it("keeps identity fields when changing from STDIO to Streamable HTTP", async () => {
    const user = userEvent.setup();
    const { getServers } = renderSettings([server({ name: "Stable name", description: "Stable description" })]);
    await openDetail(user, "Stable name");
    await user.selectOptions(screen.getByLabelText("Type"), "streamable_http");

    expect(screen.getByLabelText("Name")).toHaveValue("Stable name");
    expect(screen.getByLabelText("Description")).toHaveValue("Stable description");
    expect(screen.getByLabelText("URL")).toBeInTheDocument();
    expect(getServers()[0]).toMatchObject({
      name: "Stable name",
      description: "Stable description",
      transport: "streamable_http"
    });
  });

  it("round-trips tool enablement through disabledTools", async () => {
    const user = userEvent.setup();
    runtimeMocks.probeMcpServer.mockResolvedValue(report({
      tools: [{
        name: "lookup",
        title: "Lookup",
        description: "Looks up a record",
        requiresUserInteraction: false,
        inputSchema: { type: "object", properties: { query: { type: "string" } }, required: ["query"] }
      }]
    }));
    const { getServers } = renderSettings([server()]);

    await openDetail(user, "Alpha");
    await user.click(screen.getByRole("button", { name: "Test connection" }));
    await user.click(await screen.findByRole("tab", { name: "Tools (1)" }));
    const enable = screen.getByRole("switch", { name: "Enable tool lookup" });
    await user.click(enable);
    expect(getServers()[0].disabledTools).toEqual(["lookup"]);
    expect(enable).toHaveAttribute("aria-checked", "false");

    await user.click(enable);
    expect(getServers()[0].disabledTools).toEqual([]);
    expect(enable).toHaveAttribute("aria-checked", "true");
  });

  it("keeps the list visible when a row's Switch is clicked", async () => {
    const user = userEvent.setup();
    const { getServers } = renderSettings([
      server(),
      server({ id: "mcp-two", name: "Beta", sortOrder: 1 })
    ]);

    await user.click(screen.getByRole("switch", { name: "Beta enabled state" }));

    await waitFor(() => expect(getServers()[1].enabled).toBe(false));
    // An inline switch must not open its row's detail view; both controls share the 46px row.
    expect(screen.getByRole("button", { name: "Alpha" })).toBeInTheDocument();
    expect(screen.queryByLabelText("Name")).not.toBeInTheDocument();
  });

  it("only offers capability tabs after a successful probe", async () => {
    const user = userEvent.setup();
    runtimeMocks.probeMcpServer.mockResolvedValue(report({
      prompts: [{ name: "summarize", title: "Summarize", description: "", arguments: [] }],
      resources: [{
        uri: "file:///notes.md",
        name: "Notes",
        title: "",
        description: "",
        mimeType: "text/markdown",
        size: 2048
      }]
    }));
    renderSettings([server()]);

    await openDetail(user, "Alpha");
    expect(screen.queryByRole("tab", { name: /Prompts/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: /Resources/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Test connection" }));
    await user.click(await screen.findByRole("tab", { name: "Prompts (1)" }));
    expect(screen.getByText("Summarize")).toBeInTheDocument();

    await user.click(screen.getByRole("tab", { name: "Resources (1)" }));
    expect(screen.getByText("Notes (file:///notes.md)")).toBeInTheDocument();
  });

  it("shows the process stderr a failed probe returned", async () => {
    const user = userEvent.setup();
    runtimeMocks.probeMcpServer.mockResolvedValue(report({
      ok: false,
      protocolVersion: "",
      serverName: "",
      serverVersion: "",
      logs: ["Error: cannot find module 'mcp-server-example'"],
      error: "MCP stdio 进程已关闭输出"
    }));
    renderSettings([server()]);

    await openDetail(user, "Alpha");
    await user.click(screen.getByRole("button", { name: "Test connection" }));
    // Failed probes have no capability tabs, but their logs must remain available because
    // a failed stdio server reports its startup failure there.
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("MCP stdio 进程已关闭输出"));
    expect(screen.queryByRole("tab", { name: /Tools/ })).not.toBeInTheDocument();
    await user.click(screen.getByRole("tab", { name: "Logs" }));
    expect(screen.getByText(/cannot find module/)).toBeInTheDocument();
  });

  /**
   * A probe is a live connection the user must be able to stop: a stdio probe owns
   * a child process, and an unreachable address otherwise holds the page for the
   * whole request timeout.
   */
  function deferredReport() {
    let settle: (value: McpProbeReport) => void = () => undefined;
    const promise = new Promise<McpProbeReport>((resolve) => {
      settle = resolve;
    });
    return { promise, settle };
  }

  /** Starts a probe that stays in flight and returns the id the host was given. */
  async function startPendingProbe(user: ReturnType<typeof userEvent.setup>) {
    const pending = deferredReport();
    runtimeMocks.probeMcpServer.mockReturnValue(pending.promise);
    await openDetail(user, "Alpha");
    await user.click(screen.getByRole("button", { name: "Test connection" }));
    await screen.findByRole("button", { name: "Cancel probe" });
    const probeId = runtimeMocks.probeMcpServer.mock.calls[0][1] as string;
    // The host rejects ids it cannot bound, so the renderer must mint one it accepts.
    expect(probeId).toMatch(/^[A-Za-z0-9_-]{1,128}$/);
    return { pending, probeId };
  }

  it("cancels a running probe by the id the host was given", async () => {
    const user = userEvent.setup();
    renderSettings([server()]);
    const { pending, probeId } = await startPendingProbe(user);

    await user.click(screen.getByRole("button", { name: "Cancel probe" }));

    expect(runtimeMocks.cancelMcpProbe).toHaveBeenCalledWith(probeId);
    // The card returns to "not probed": a cancelled probe reached no verdict.
    expect(await screen.findByRole("button", { name: "Test connection" })).toBeEnabled();

    // The abandoned probe's late answer must not land on the page.
    pending.settle(report({ ok: false, error: "MCP stdio process closed its output" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(screen.queryByText(/closed its output/)).not.toBeInTheDocument();
  });

  it("shows a refused cancel instead of leaving a button that did nothing", async () => {
    const user = userEvent.setup();
    renderSettings([server()]);
    await startPendingProbe(user);
    runtimeMocks.cancelMcpProbe.mockRejectedValue(new Error("MCP probe id is already in flight"));

    await user.click(screen.getByRole("button", { name: "Cancel probe" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("MCP probe id is already in flight");
  });

  it("does not report a probe the host cancelled as a failed connection", async () => {
    const user = userEvent.setup();
    runtimeMocks.probeMcpServer.mockResolvedValue(report({
      ok: false,
      cancelled: true,
      protocolVersion: "",
      serverName: "",
      serverVersion: "",
      error: "MCP probe was cancelled"
    }));
    renderSettings([server()]);

    await openDetail(user, "Alpha");
    await user.click(screen.getByRole("button", { name: "Test connection" }));

    await waitFor(() => expect(screen.getByRole("button", { name: "Test connection" })).toBeEnabled());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.queryByText("MCP probe was cancelled")).not.toBeInTheDocument();
  });

  it("ends a probe whose server the user deleted", async () => {
    const user = userEvent.setup();
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    try {
      const { getServers } = renderSettings([server()]);
      const { probeId } = await startPendingProbe(user);

      await user.click(screen.getByRole("button", { name: "Delete" }));

      expect(getServers()).toHaveLength(0);
      expect(runtimeMocks.cancelMcpProbe).toHaveBeenCalledWith(probeId);
    } finally {
      confirm.mockRestore();
    }
  });

  it("ends a probe the page left behind when it closed", async () => {
    const user = userEvent.setup();
    const { unmount } = renderSettings([server()]);
    const { probeId } = await startPendingProbe(user);

    unmount();

    expect(runtimeMocks.cancelMcpProbe).toHaveBeenCalledWith(probeId);
  });

  it("offers a package registry mirror only for a recognised toolchain", async () => {    const user = userEvent.setup();
    const { getServers } = renderSettings([server({ command: "npx" })]);

    await openDetail(user, "Alpha");
    const mirror = screen.getByRole("radio", { name: "Taobao NPM Mirror" });
    await user.click(mirror);
    expect(getServers()[0].registryUrl).toBe("https://registry.npmmirror.com");

    // An unrecognized command makes the mirror irrelevant, so it must not persist.
    const command = screen.getByLabelText("Command");
    await user.clear(command);
    await user.type(command, "node");
    expect(screen.queryByRole("radio", { name: "Taobao NPM Mirror" })).not.toBeInTheDocument();
    expect(getServers()[0].registryUrl).toBe("");
  });

  it("keeps the market page reachable without any configured server", async () => {
    const user = userEvent.setup();
    renderSettings([]);

    expect(screen.getByText("No servers configured")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Marketplaces" }));
    expect(screen.getByRole("heading", { name: "Find more MCP servers" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /mcp\.so/ })).toHaveAttribute("href", "https://mcp.so/");
  });
});

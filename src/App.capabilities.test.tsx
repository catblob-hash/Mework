import { render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App, {
  mergeResourceOrder,
  reorderDocumentResources
} from "./App";
import { createTestDocument as createSeedDocument } from "./test/fixtures";
import { configureI18n } from "./i18n";
import type {
  ResourceDescriptor
} from "./types";
import { resetAppMocks, browserRendererMountMocks, runtimeMocks } from "./test/appMocks";

vi.mock("./lib/runtime", async (importOriginal) => {
  const { runtimeMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/runtime")>(), ...runtimeMocks };
});
vi.mock("./lib/terminal", async () => (await import("./test/appMockInstances")).terminalMocks);
vi.mock("./lib/browser", async () => (await import("./test/appMockInstances")).browserMocks);
vi.mock("./lib/browserRendererMount", async () => {
  const { browserRendererMountMocks } = await import("./test/appMockInstances");
  return {
    startBrowserRendererMountHeartbeat: browserRendererMountMocks.startHeartbeat,
    stopBrowserRendererMountHeartbeat: browserRendererMountMocks.stopHeartbeat
  };
});
vi.mock("./lib/git", async (importOriginal) => {
  const { gitMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/git")>(), ...gitMocks };
});
vi.mock("./components/TerminalPanel", async () => (await import("./test/appMockInstances")).terminalPanelModuleMock());

afterEach(() => configureI18n("zh-CN"));

describe("App model run flow — capabilities", () => {
  beforeEach(resetAppMocks);

  it("keeps one native renderer mount heartbeat for the App lifetime", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    runtimeMocks.loadDocument.mockResolvedValue(createSeedDocument());
    const view = render(<App />);

    await waitFor(() => {
      expect(browserRendererMountMocks.startHeartbeat).toHaveBeenCalledTimes(1);
    });
    view.unmount();

    expect(browserRendererMountMocks.stopHeartbeat).toHaveBeenCalledTimes(1);
  });

  it("reorders the capability catalog without touching direct resource references", () => {
    const document = createSeedDocument();
    const template = document.capabilities.skills[0];
    document.capabilities.hooks = ["hook-a", "hook-b", "hook-c"].map((id) => ({
      ...template,
      id,
      name: id
    }));
    const conversation = document.workspaces[0].conversations[0];
    conversation.settings.hookIds = ["hook-a", "hook-c"];
    document.globalSettings.conversationPresets[0].settings.hookIds = ["hook-b", "hook-a"];

    const reordered = reorderDocumentResources(document, "hooks", "hook-c", "hook-a", "before");

    expect(reordered.capabilities.hooks.map((resource) => resource.id)).toEqual(["hook-c", "hook-a", "hook-b"]);
    // Catalog order is display-only: conversations and presets retain direct resource IDs.
    expect(reordered.workspaces[0].conversations[0].settings.hookIds).toEqual(["hook-a", "hook-c"]);
    expect(reordered.globalSettings.conversationPresets[0].settings.hookIds).toEqual(["hook-b", "hook-a"]);
  });

  it("keeps a custom resource order when a scan updates entries and adds new ones", () => {
    const descriptor = (id: string, name: string): ResourceDescriptor => ({
      id,
      name,
      description: "",
      location: `test://skills/${id}/SKILL.md`,
      source: "user",
      available: true
    });
    const first = descriptor("skill-a", "技能 A");
    const second = descriptor("skill-b", "技能 B");
    const merged = mergeResourceOrder(
      [second, first],
      [{ ...first, description: "已更新" }, { ...second, description: "也已更新" }, descriptor("skill-new", "新技能")]
    );

    expect(merged.map((resource) => resource.id)).toEqual([second.id, first.id, "skill-new"]);
    expect(merged[1].description).toBe("已更新");
  });
});

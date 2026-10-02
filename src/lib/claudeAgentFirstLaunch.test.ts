import { beforeEach, describe, expect, it, vi } from "vitest";
import { createSeedDocument } from "../seed";
import type { AppDocument, ClaudeAgentLoginStatus, ModelProfile } from "../types";

const remote = vi.hoisted(() => ({
  claudeAgentLoginStatus: vi.fn(),
  fetchModels: vi.fn()
}));
vi.mock("./runtime", () => remote);

const firstLaunch = () => import("./claudeAgentFirstLaunch");

function claudeRow(document: AppDocument) {
  const row = document.globalSettings.apiProviders.find((provider) => provider.family === "claude_agent");
  if (!row) throw new Error("the seed has a Claude Agent row");
  return row;
}

/** Rows shaped as the host's fetch projects them, under the ids Claude Code's picker resolves to. */
function fetched(...ids: string[]): ModelProfile[] {
  const template = claudeRow(createSeedDocument()).models[0];
  return ids.map((id) => ({ ...template, id, name: id }));
}

/** A real login's picker: newest first per line, older models, a dated Haiku and an alias. */
const PICKER = [
  "claude-opus-5-5",
  "claude-fable-5-1",
  "claude-sonnet-5",
  "claude-haiku-4-5-20251001",
  "claude-fable-5",
  "claude-opus-5",
  "claude-opus-4-8",
  "claude-opus-4-20250514",
  "claude-sonnet-4-6",
  "sonnet"
];

function signedIn(signed: boolean): ClaudeAgentLoginStatus {
  return {
    signedIn: signed,
    authMethod: signed ? "claude.ai" : "none",
    email: null,
    orgName: null,
    subscriptionType: null,
    executable: "claude",
    configDir: "~/.claude"
  };
}

beforeEach(() => {
  vi.resetModules();
  remote.claudeAgentLoginStatus.mockReset();
  remote.fetchModels.mockReset();
});

describe("newestClaudeModelPerLine", () => {
  it("keeps the newest of fable, opus, sonnet and haiku, in that order", async () => {
    const { newestClaudeModelPerLine } = await firstLaunch();
    expect(newestClaudeModelPerLine(fetched(...PICKER)).map((model) => model.id)).toEqual([
      "claude-fable-5-1",
      "claude-opus-5-5",
      "claude-sonnet-5",
      "claude-haiku-4-5-20251001"
    ]);
  });

  it("compares versions, not list order, and reads a bare dated id as the major alone", async () => {
    const { newestClaudeModelPerLine } = await firstLaunch();
    expect(newestClaudeModelPerLine(fetched(
      "claude-opus-4-20250514",
      "claude-opus-4-1",
      "claude-sonnet-4-5",
      "claude-sonnet-4-10"
    )).map((model) => model.id)).toEqual(["claude-opus-4-1", "claude-sonnet-4-10"]);
  });

  it("lets the first listed of one version win, and skips what carries no version", async () => {
    const { newestClaudeModelPerLine } = await firstLaunch();
    expect(newestClaudeModelPerLine(fetched("claude-haiku-4-5", "claude-haiku-4-5-20251001", "opus", "default"))
      .map((model) => model.id)).toEqual(["claude-haiku-4-5"]);
  });
});

describe("adoptFirstLaunchClaudeModels", () => {
  it("loads the four newest into the Claude Agent row and selects its Opus", async () => {
    const { adoptFirstLaunchClaudeModels } = await firstLaunch();
    const seed = createSeedDocument();
    const row = claudeRow(seed);
    const adopted = adoptFirstLaunchClaudeModels(
      { ...seed, globalSettings: { ...seed.globalSettings, activeProviderId: null } },
      fetched(...PICKER)
    );

    expect(adopted).not.toBeNull();
    const adoptedRow = claudeRow(adopted!);
    expect(adoptedRow.id).toBe(row.id);
    expect(adoptedRow.enabled).toBe(true);
    expect(adoptedRow.models.map((model) => model.id)).toEqual([
      "claude-fable-5-1",
      "claude-opus-5-5",
      "claude-sonnet-5",
      "claude-haiku-4-5-20251001"
    ]);
    expect(adoptedRow.activeModelId).toBe("claude-opus-5-5");
    expect(adopted!.globalSettings.activeProviderId).toBe(row.id);
    // The other rows are untouched.
    expect(adopted!.globalSettings.apiProviders.filter((provider) => provider.id !== row.id))
      .toEqual(seed.globalSettings.apiProviders.filter((provider) => provider.id !== row.id));
  });

  it("selects the first adopted model when the list has no Opus", async () => {
    const { adoptFirstLaunchClaudeModels } = await firstLaunch();
    const adopted = adoptFirstLaunchClaudeModels(createSeedDocument(), fetched("claude-sonnet-5", "claude-haiku-4-5"));
    expect(claudeRow(adopted!).activeModelId).toBe("claude-sonnet-5");
  });

  it("adopts nothing from a list without a versioned Claude model", async () => {
    const { adoptFirstLaunchClaudeModels } = await firstLaunch();
    expect(adoptFirstLaunchClaudeModels(createSeedDocument(), [])).toBeNull();
    expect(adoptFirstLaunchClaudeModels(createSeedDocument(), fetched("sonnet", "opus"))).toBeNull();
  });
});

describe("setUpClaudeAgentOnFirstLaunch", () => {
  function store(initial: AppDocument) {
    let current: AppDocument | null = initial;
    const update = vi.fn((updater: (document: AppDocument | null) => AppDocument | null) => {
      current = updater(current);
    });
    return { update, current: () => current };
  }

  it("asks the CLI only when it is signed in, then adopts what it lists", async () => {
    const { setUpClaudeAgentOnFirstLaunch } = await firstLaunch();
    remote.claudeAgentLoginStatus.mockResolvedValue(signedIn(true));
    remote.fetchModels.mockResolvedValue(fetched(...PICKER));
    const seed = createSeedDocument();
    const documents = store(seed);

    await setUpClaudeAgentOnFirstLaunch(seed, documents.update);

    expect(remote.claudeAgentLoginStatus).toHaveBeenCalledWith(claudeRow(seed));
    expect(remote.fetchModels).toHaveBeenCalledWith(claudeRow(seed));
    expect(claudeRow(documents.current()!).activeModelId).toBe("claude-opus-5-5");
    expect(documents.current()!.globalSettings.activeProviderId).toBe(claudeRow(seed).id);
  });

  it("leaves the seed alone when the CLI is signed out, fails, or lists nothing", async () => {
    for (const arrange of [
      () => remote.claudeAgentLoginStatus.mockResolvedValue(signedIn(false)),
      () => remote.claudeAgentLoginStatus.mockRejectedValue(new Error("timed out")),
      () => {
        remote.claudeAgentLoginStatus.mockResolvedValue(signedIn(true));
        remote.fetchModels.mockRejectedValue(new Error("CLI failed"));
      },
      () => {
        remote.claudeAgentLoginStatus.mockResolvedValue(signedIn(true));
        remote.fetchModels.mockResolvedValue([]);
      }
    ]) {
      vi.resetModules();
      remote.claudeAgentLoginStatus.mockReset();
      remote.fetchModels.mockReset();
      arrange();
      const { setUpClaudeAgentOnFirstLaunch } = await firstLaunch();
      const seed = createSeedDocument();
      const documents = store(seed);

      await setUpClaudeAgentOnFirstLaunch(seed, documents.update);

      expect(documents.current()).toBe(seed);
    }
  });

  it("keeps a model the user picked while the CLI was answering", async () => {
    const { setUpClaudeAgentOnFirstLaunch } = await firstLaunch();
    let answer: (models: ModelProfile[]) => void = () => {};
    remote.claudeAgentLoginStatus.mockResolvedValue(signedIn(true));
    remote.fetchModels.mockReturnValue(new Promise((resolve) => { answer = resolve; }));
    const seed = createSeedDocument();
    const documents = store(seed);

    const running = setUpClaudeAgentOnFirstLaunch(seed, documents.update);
    const row = claudeRow(seed);
    const picked: AppDocument = {
      ...seed,
      globalSettings: {
        ...seed.globalSettings,
        apiProviders: seed.globalSettings.apiProviders.map((provider) => (
          provider.id === row.id ? { ...provider, activeModelId: "claude-sonnet-5" } : provider
        ))
      }
    };
    documents.update(() => picked);
    await vi.waitFor(() => expect(remote.fetchModels).toHaveBeenCalled());
    answer(fetched(...PICKER));
    await running;

    expect(documents.current()).toBe(picked);
  });

  it("runs once per renderer", async () => {
    const { setUpClaudeAgentOnFirstLaunch } = await firstLaunch();
    remote.claudeAgentLoginStatus.mockResolvedValue(signedIn(false));
    const seed = createSeedDocument();
    const documents = store(seed);

    await setUpClaudeAgentOnFirstLaunch(seed, documents.update);
    await setUpClaudeAgentOnFirstLaunch(seed, documents.update);

    expect(remote.claudeAgentLoginStatus).toHaveBeenCalledTimes(1);
  });
});

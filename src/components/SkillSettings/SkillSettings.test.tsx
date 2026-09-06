import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../../i18n";
import type { SkillRecord } from "../../types";

const runtimeMocks = vi.hoisted(() => ({
  installSkillFromArchive: vi.fn(),
  installSkillFromDirectory: vi.fn(),
  installSkillFromRegistry: vi.fn(),
  searchSkillRegistries: vi.fn(),
  scanSystemSkills: vi.fn(),
  uninstallSkill: vi.fn()
}));

vi.mock("../../lib/backend", () => ({ hasBackendRuntime: () => true, isTauriRuntime: () => false }));
vi.mock("../../lib/runtime", () => runtimeMocks);

import { SkillSettings } from ".";

function skill(overrides: Partial<SkillRecord> = {}): SkillRecord {
  return {
    id: "skill-translator",
    name: "Translator",
    description: "Translates product copy while preserving terminology.",
    folderName: "translator",
    source: "local_directory",
    sourceLocation: "/skills/translator",
    sourceUrl: "",
    author: "Mework",
    version: "1.2.0",
    tags: ["writing", "translation"],
    contentHash: "hash",
    enabled: true,
    installedAt: "2026-08-20T10:00:00.000Z",
    updatedAt: "2026-08-21T10:00:00.000Z",
    ...overrides
  };
}

function renderSettings(initial: SkillRecord[]) {
  let current = initial;
  const onChange = vi.fn();

  function Harness() {
    const [skills, setSkills] = useState(initial);
    current = skills;
    return (
      <SkillSettings
        skills={skills}
        onChange={(next) => {
          onChange(next);
          setSkills(next);
        }}
      />
    );
  }

  return { ...render(<Harness />), getSkills: () => current, onChange };
}

async function openAddMenu(user: ReturnType<typeof userEvent.setup>, item: string) {
  await user.click(screen.getByRole("button", { name: "Add skill" }));
  await user.click(screen.getByRole("menuitem", { name: item }));
}

describe("SkillSettings", () => {
  beforeEach(() => {
    configureI18n("en-US");
    runtimeMocks.installSkillFromArchive.mockReset().mockResolvedValue(null);
    runtimeMocks.installSkillFromDirectory.mockReset().mockResolvedValue(null);
    runtimeMocks.installSkillFromRegistry.mockReset().mockResolvedValue(skill());
    runtimeMocks.searchSkillRegistries.mockReset().mockResolvedValue({ results: [], failedSources: [] });
    runtimeMocks.scanSystemSkills.mockReset().mockResolvedValue([]);
    runtimeMocks.uninstallSkill.mockReset().mockResolvedValue(undefined);
  });

  afterEach(() => configureI18n("zh-CN"));

  it("toggles global enablement without opening details", async () => {
    const user = userEvent.setup();
    const { getSkills } = renderSettings([skill()]);

    await user.click(screen.getByRole("switch", { name: "Enable Translator globally" }));

    expect(getSkills()[0].enabled).toBe(false);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("opens uninstall confirmation from the trash action without opening details", async () => {
    const user = userEvent.setup();
    renderSettings([skill()]);

    await user.click(screen.getByRole("button", { name: "Uninstall Translator" }));

    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByRole("heading", { name: "Uninstall skill" })).toBeInTheDocument();
    expect(screen.queryByText("Skill details")).not.toBeInTheDocument();
  });

  it("opens the detail dialog with Enter on the focused card", async () => {
    const user = userEvent.setup();
    renderSettings([skill()]);
    const card = screen.getByRole("button", { name: "View skill Translator" });

    card.focus();
    await user.keyboard("{Enter}");

    expect(screen.getByRole("heading", { name: "Skill details" })).toBeInTheDocument();
  });

  it("distinguishes an empty registry from an empty search result", async () => {
    const user = userEvent.setup();
    const empty = renderSettings([]);
    expect(screen.getByText("No skills yet")).toBeInTheDocument();
    expect(screen.queryByText("No matching skills")).not.toBeInTheDocument();
    empty.unmount();

    renderSettings([skill()]);
    await user.click(screen.getByRole("button", { name: "Search skills" }));
    await user.type(screen.getByRole("textbox", { name: "Search skills" }), "unmatched");

    expect(screen.getByText("No matching skills")).toBeInTheDocument();
    expect(screen.getByText("Try another keyword.")).toBeInTheDocument();
    expect(screen.queryByText("No skills yet")).not.toBeInTheDocument();
  });

  it("keeps the skill when uninstalling its files fails", async () => {
    const user = userEvent.setup();
    runtimeMocks.uninstallSkill.mockRejectedValue(new Error("disk is busy"));
    const { getSkills, onChange } = renderSettings([skill()]);

    await user.click(screen.getByRole("button", { name: "Uninstall Translator" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Uninstall" }));

    await waitFor(() => expect(runtimeMocks.uninstallSkill).toHaveBeenCalledWith("translator"));
    expect(await screen.findByText("Uninstall failed and the skill was kept. Try again.")).toBeInTheDocument();
    expect(getSkills()).toHaveLength(1);
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "View skill Translator" })).toBeInTheDocument();
  });

  it("installs a registry hit and folds it into the local registry", async () => {
    const user = userEvent.setup();
    runtimeMocks.searchSkillRegistries.mockResolvedValue({
      results: [{
        slug: "owner/repo/pdf",
        name: "pdf-tools",
        description: "Fill and split PDFs",
        author: "owner",
        stars: 12,
        downloads: 340,
        sourceRegistry: "skills.sh",
        sourceUrl: "https://skills.sh/owner/repo/pdf",
        installSource: "skills.sh:owner/repo/pdf"
      }],
      failedSources: []
    });
    runtimeMocks.installSkillFromRegistry.mockResolvedValue(
      skill({ id: "skill-pdf", name: "pdf-tools", folderName: "pdf-tools", source: "remote" })
    );
    const { getSkills } = renderSettings([]);

    await openAddMenu(user, "Search online");
    await user.type(screen.getByRole("textbox", { name: "Search skills" }), "pdf");
    await waitFor(() => expect(runtimeMocks.searchSkillRegistries).toHaveBeenCalledWith("pdf"));

    await user.click(await screen.findByRole("button", { name: "Install" }));
    await waitFor(() => expect(runtimeMocks.installSkillFromRegistry)
      .toHaveBeenCalledWith("skills.sh:owner/repo/pdf"));
    await waitFor(() => expect(getSkills()).toHaveLength(1));
    expect(getSkills()[0].name).toBe("pdf-tools");
  });

  it("filters registry hits by the selected source without searching again", async () => {
    const user = userEvent.setup();
    runtimeMocks.searchSkillRegistries.mockResolvedValue({
      results: [
        {
          slug: "a", name: "from-skills-sh", description: "", author: "a", stars: 0, downloads: 0,
          sourceRegistry: "skills.sh", sourceUrl: "", installSource: "skills.sh:a/b/c"
        },
        {
          slug: "b", name: "from-clawhub", description: "", author: "b", stars: 0, downloads: 0,
          sourceRegistry: "clawhub.ai", sourceUrl: "", installSource: "clawhub:b/c"
        }
      ],
      failedSources: []
    });
    renderSettings([]);

    await openAddMenu(user, "Search online");
    await user.type(screen.getByRole("textbox", { name: "Search skills" }), "x");
    expect(await screen.findByText("from-skills-sh")).toBeInTheDocument();
    expect(screen.queryByText("from-clawhub")).not.toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText("Skill source"), "clawhub.ai");
    expect(screen.getByText("from-clawhub")).toBeInTheDocument();
    expect(screen.queryByText("from-skills-sh")).not.toBeInTheDocument();
    expect(runtimeMocks.searchSkillRegistries).toHaveBeenCalledTimes(1);
  });

  it("validates a GitHub link locally instead of searching for it", async () => {
    const user = userEvent.setup();
    renderSettings([]);

    await openAddMenu(user, "Search online");
    await user.selectOptions(screen.getByLabelText("Skill source"), "github");
    const input = screen.getByRole("textbox", { name: "GitHub SKILL.md link" });

    await user.type(input, "https://github.com/owner/repo/tree/main/skills/demo");
    expect(await screen.findByRole("alert")).toHaveTextContent("points directly at a SKILL.md file");

    await user.clear(input);
    await user.type(input, "https://github.com/owner/repo/blob/main/skills/demo/SKILL.md");
    expect(await screen.findByText("repo")).toBeInTheDocument();
    expect(runtimeMocks.searchSkillRegistries).not.toHaveBeenCalled();
  });

  it("reports partial registry failures without hiding the hits that arrived", async () => {
    const user = userEvent.setup();
    runtimeMocks.searchSkillRegistries.mockResolvedValue({
      results: [{
        slug: "a", name: "survivor", description: "", author: "a", stars: 0, downloads: 0,
        sourceRegistry: "skills.sh", sourceUrl: "", installSource: "skills.sh:a/b/c"
      }],
      failedSources: ["clawhub.ai"]
    });
    renderSettings([]);

    await openAddMenu(user, "Search online");
    await user.type(screen.getByRole("textbox", { name: "Search skills" }), "x");

    expect(await screen.findByText("survivor")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("clawhub.ai");
  });
});

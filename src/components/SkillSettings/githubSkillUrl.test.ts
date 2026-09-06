import { describe, expect, it } from "vitest";
import { buildGithubResult, parseGithubSkillUrl } from "./githubSkillUrl";

/**
 * These assertions match the identically named tests in `src-tauri/src/skill_registry.rs`.
 * The UI must accept exactly the URLs the installer can clone; otherwise a validated
 * link fails during installation without exposing the differing rule.
 */

describe("parseGithubSkillUrl", () => {
  it("requires a link that points at a skill manifest", () => {
    expect(parseGithubSkillUrl("https://github.com/owner/repo")).toBeNull();
    expect(parseGithubSkillUrl("https://github.com/owner/repo/tree/main/skills/demo")).toBeNull();
    expect(parseGithubSkillUrl("https://github.com/owner/repo/blob/main/skills/demo/SKILL.md")).toEqual({
      owner: "owner",
      repo: "repo",
      refNamespace: null,
      refAndPath: ["main", "skills", "demo"],
      descriptorFileName: "SKILL.md"
    });
  });

  it("keeps the ref namespace a raw-content link carries", () => {
    expect(parseGithubSkillUrl(
      "https://raw.githubusercontent.com/owner/repo/refs/heads/main/demo/SKILL.md"
    )).toMatchObject({ refNamespace: "heads", refAndPath: ["main", "demo"] });
  });

  it("rejects traversal, foreign hosts, and a bare repository", () => {
    expect(parseGithubSkillUrl("https://github.com/owner/repo/blob/main/../SKILL.md")).toBeNull();
    expect(parseGithubSkillUrl("https://github.com/owner/repo/blob/main/%2e%2e/SKILL.md")).toBeNull();
    expect(parseGithubSkillUrl("https://example.com/owner/repo/blob/main/SKILL.md")).toBeNull();
    expect(parseGithubSkillUrl("https://github.com/owner/repo/blob/SKILL.md")).toBeNull();
    expect(parseGithubSkillUrl("not a url")).toBeNull();
  });

  it("accepts a lowercase manifest name and a .git suffix", () => {
    expect(parseGithubSkillUrl("https://github.com/owner/repo.git/blob/main/demo/skill.md"))
      .toMatchObject({ repo: "repo", descriptorFileName: "skill.md" });
  });
});

describe("buildGithubResult", () => {
  it("round-trips its own install handle back through the parser", () => {
    const result = buildGithubResult("https://github.com/owner/repo/blob/main/skills/demo/SKILL.md");
    expect(result).toMatchObject({ name: "repo", author: "owner", sourceRegistry: "github" });
    const handle = result?.installSource.replace(/^github:/, "") ?? "";
    expect(parseGithubSkillUrl(handle)).not.toBeNull();
  });

  it("canonicalises a raw-content link back to the raw form", () => {
    const result = buildGithubResult(
      "https://raw.githubusercontent.com/owner/repo/refs/tags/v1/demo/SKILL.md"
    );
    expect(result?.sourceUrl).toBe(
      "https://raw.githubusercontent.com/owner/repo/refs/tags/v1/demo/SKILL.md"
    );
  });

  it("returns null for anything the parser rejects", () => {
    expect(buildGithubResult("https://github.com/owner/repo")).toBeNull();
  });
});

import type { SkillSearchResult } from "../../types";

/**
 * Parses direct GitHub SKILL.md URLs.
 *
 * This mirrors Rust's `skill_registry.rs::parse_github_skill_url`: every URL
 * accepted here must be cloneable by the installer. Both implementations use
 * the same rules and independent tests:
 *
 * - Accept only `github.com/{owner}/{repo}/blob|raw/...` and
 *   `raw.githubusercontent.com/...`.
 * - Require a `SKILL.md` or `skill.md` suffix. A bare repository or tree URL
 *   leaves the installer unable to identify the skill directory.
 * - Decoded path segments must not contain `.`, `..`, `/`, `\`, or NUL.
 *
 * The remote resolves the boundary between the ref and directory path, so this
 * parser preserves them together; the host splits them with `git ls-remote`.
 */

const REPO_PART = /^[a-zA-Z0-9_.-]+$/;

function invalidPathPart(part: string): boolean {
  return !part
    || part !== part.trim()
    || part === "."
    || part === ".."
    || part.includes("\\")
    || part.includes("/")
    || part.includes("\0");
}

export interface GithubSkillLocation {
  owner: string;
  repo: string;
  refNamespace: "heads" | "tags" | null;
  /** Decoded `ref + directory path` segments. */
  refAndPath: string[];
  descriptorFileName: "SKILL.md" | "skill.md";
}

export function parseGithubSkillUrl(rawUrl: string): GithubSkillLocation | null {
  let url: URL;
  let segments: string[];
  try {
    url = new URL(rawUrl.trim());
    if (url.protocol !== "https:" && url.protocol !== "http:") return null;
    segments = url.pathname.split("/").filter(Boolean).map(decodeURIComponent);
  } catch {
    return null;
  }
  if (segments.some(invalidPathPart)) return null;

  const host = url.hostname.toLowerCase().replace(/^www\./, "");
  const [owner, rawRepo, ...tail] = segments;
  const rawRefAndPath = host === "github.com" && (tail[0] === "blob" || tail[0] === "raw")
    ? tail.slice(1)
    : host === "raw.githubusercontent.com"
      ? tail
      : null;
  if (!rawRefAndPath) return null;

  const repo = rawRepo?.replace(/\.git$/i, "") ?? "";
  const fileName = rawRefAndPath.at(-1);
  const descriptorFileName = fileName === "SKILL.md" || fileName === "skill.md" ? fileName : null;
  let refAndPath = rawRefAndPath.slice(0, -1);
  let refNamespace: GithubSkillLocation["refNamespace"] = null;
  if (refAndPath[0] === "refs" && (refAndPath[1] === "heads" || refAndPath[1] === "tags")) {
    refNamespace = refAndPath[1];
    refAndPath = refAndPath.slice(2);
  }

  if (!owner || !repo || !descriptorFileName) return null;
  if (!REPO_PART.test(owner) || !REPO_PART.test(repo)) return null;
  // At least one segment is required; it may be a ref selecting the repository root.
  if (!refAndPath.length) return null;

  return { owner, repo, refNamespace, refAndPath, descriptorFileName };
}

/** Presents a validated direct URL as an installable search result. */
export function buildGithubResult(rawUrl: string): SkillSearchResult | null {
  const location = parseGithubSkillUrl(rawUrl);
  if (!location) return null;
  const path = location.refAndPath.join("/");
  const namespace = location.refNamespace ? `refs/${location.refNamespace}/` : "";
  const canonical = location.refNamespace
    ? `https://raw.githubusercontent.com/${location.owner}/${location.repo}/${namespace}${path}/${location.descriptorFileName}`
    : `https://github.com/${location.owner}/${location.repo}/blob/${path}/${location.descriptorFileName}`;
  return {
    slug: `${location.owner}/${location.repo}/${namespace}${path}`,
    name: location.repo,
    description: "",
    author: location.owner,
    stars: 0,
    downloads: 0,
    sourceRegistry: "github",
    sourceUrl: canonical,
    installSource: `github:${canonical}`
  };
}

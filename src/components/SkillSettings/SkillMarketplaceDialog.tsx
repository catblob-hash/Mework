import { Check, Download, ExternalLink, LoaderCircle, Search, Star, X } from "lucide-react";
import type { JSX } from "react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import { hasBackendRuntime } from "../../lib/backend";
import { installSkillFromRegistry, searchSkillRegistries } from "../../lib/runtime";
import type { SkillRecord, SkillSearchResult, SkillSearchSource } from "../../types";
import { Dialog, IconButton } from "../Common";
import { buildGithubResult } from "./githubSkillUrl";

/**
 * Online skill search.
 *
 * Registry searches run concurrently in the host because the renderer cannot reach
 * them under CSP. GitHub accepts and validates a direct SKILL.md URL. Switching
 * registry sources filters shared results; switching to or from GitHub clears
 * incompatible input.
 */

const SOURCE_LABELS: Record<SkillSearchSource, string> = {
  "skills.sh": "skills.sh",
  "claude-plugins.dev": "claude-plugins.dev",
  "clawhub.ai": "clawhub.ai",
  github: "GitHub"
};
const SEARCH_SOURCES = Object.keys(SOURCE_LABELS) as SkillSearchSource[];
const DEFAULT_SOURCE: SkillSearchSource = "skills.sh";
const SEARCH_DEBOUNCE_MS = 300;

export function SkillMarketplaceDialog({
  onClose,
  onInstalled
}: {
  onClose: () => void;
  onInstalled: (skill: SkillRecord) => void;
}): JSX.Element {
  const { t } = useI18n();
  const backendAvailable = hasBackendRuntime();
  const [query, setQuery] = useState("");
  const [submittedUrl, setSubmittedUrl] = useState("");
  const [source, setSource] = useState<SkillSearchSource>(DEFAULT_SOURCE);
  const [results, setResults] = useState<SkillSearchResult[]>([]);
  const [failedSources, setFailedSources] = useState<SkillSearchSource[]>([]);
  const [searching, setSearching] = useState(false);
  const [debouncing, setDebouncing] = useState(false);
  const [searchFailed, setSearchFailed] = useState(false);
  const [installing, setInstalling] = useState<string | null>(null);
  const [installed, setInstalled] = useState<Set<string>>(() => new Set());
  const [installError, setInstallError] = useState<string | null>(null);
  const debounceRef = useRef<number | null>(null);
  const requestRef = useRef(0);
  const mountedRef = useRef(true);
  const urlErrorId = useId();
  const isGithub = source === "github";

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
    };
  }, []);

  const clearPending = useCallback(() => {
    if (debounceRef.current === null) return;
    window.clearTimeout(debounceRef.current);
    debounceRef.current = null;
  }, []);

  const runSearch = useCallback(async (value: string) => {
    const requestId = requestRef.current + 1;
    requestRef.current = requestId;
    setSearching(true);
    setSearchFailed(false);
    try {
      const report = await searchSkillRegistries(value);
      if (!mountedRef.current || requestRef.current !== requestId) return;
      setResults(report.results);
      setFailedSources(report.failedSources);
      setSearchFailed(!report.results.length && report.failedSources.length > 0);
    } catch {
      if (!mountedRef.current || requestRef.current !== requestId) return;
      setResults([]);
      setSearchFailed(true);
    } finally {
      if (mountedRef.current && requestRef.current === requestId) setSearching(false);
    }
  }, []);

  const handleQueryChange = (value: string) => {
    setQuery(value);
    setSubmittedUrl("");
    clearPending();
    // Clear stale results so they cannot be mistaken for results for the new query.
    requestRef.current += 1;
    setResults([]);
    setFailedSources([]);
    setSearchFailed(false);
    if (!value.trim()) {
      setDebouncing(false);
      setSearching(false);
      return;
    }
    setDebouncing(true);
    debounceRef.current = window.setTimeout(() => {
      debounceRef.current = null;
      setDebouncing(false);
      if (isGithub) setSubmittedUrl(value);
      else void runSearch(value);
    }, SEARCH_DEBOUNCE_MS);
  };

  const handleSourceChange = (next: SkillSearchSource) => {
    setSource(next);
    if ((next === "github") === isGithub) return;
    clearPending();
    requestRef.current += 1;
    setQuery("");
    setSubmittedUrl("");
    setResults([]);
    setFailedSources([]);
    setSearchFailed(false);
    setDebouncing(false);
    setSearching(false);
  };

  const githubResult = useMemo(
    () => (isGithub && submittedUrl.trim() ? buildGithubResult(submittedUrl) : null),
    [isGithub, submittedUrl]
  );
  const githubUrlInvalid = isGithub && submittedUrl.trim().length > 0 && !githubResult;

  const counts = useMemo(() => {
    const map = new Map<SkillSearchSource, number>();
    for (const result of results) {
      map.set(result.sourceRegistry, (map.get(result.sourceRegistry) ?? 0) + 1);
    }
    if (githubResult) map.set("github", 1);
    return map;
  }, [githubResult, results]);

  const visible = isGithub
    ? (githubResult ? [githubResult] : [])
    : results.filter((result) => result.sourceRegistry === source);

  const install = async (result: SkillSearchResult) => {
    if (!backendAvailable || installing || installed.has(result.installSource)) return;
    setInstalling(result.installSource);
    setInstallError(null);
    try {
      const skill = await installSkillFromRegistry(result.installSource);
      if (!mountedRef.current) return;
      setInstalled((current) => new Set(current).add(result.installSource));
      onInstalled(skill);
    } catch (error) {
      if (!mountedRef.current) return;
      const message = error instanceof Error ? error.message : String(error);
      setInstallError(t("技能安装失败：{name} · {reason}", "Failed to install {name}: {reason}", {
        name: result.name,
        reason: message
      }));
    } finally {
      if (mountedRef.current) setInstalling(null);
    }
  };

  const activeQuery = isGithub ? (githubResult ? submittedUrl : "") : query;
  const busy = !isGithub && (searching || debouncing);

  return (
    <Dialog
      title={t("在线搜索技能", "Search skills online")}
      width="760px"
      dismissible={!installing}
      onClose={() => {
        if (!installing) onClose();
      }}
    >
      <div className="skill-market">
        <div className="skill-market__controls">
          <label className="skill-market__source">
            <span className="sr-only">{t("技能来源", "Skill source")}</span>
            <select
              className="input"
              aria-label={t("技能来源", "Skill source")}
              value={source}
              onChange={(event) => handleSourceChange(event.target.value as SkillSearchSource)}
            >
              {SEARCH_SOURCES.map((item) => {
                const count = counts.get(item) ?? 0;
                return (
                  <option value={item} key={item}>
                    {count > 0 ? `${SOURCE_LABELS[item]} (${count})` : SOURCE_LABELS[item]}
                  </option>
                );
              })}
            </select>
          </label>
          <div className="skill-market__search">
            <Search size={14} aria-hidden="true" />
            <input
              autoFocus
              value={query}
              aria-label={isGithub
                ? t("GitHub SKILL.md 链接", "GitHub SKILL.md link")
                : t("搜索技能", "Search skills")}
              aria-invalid={githubUrlInvalid || undefined}
              aria-describedby={githubUrlInvalid ? urlErrorId : undefined}
              placeholder={isGithub
                ? t("GitHub 链接，以 /SKILL.md 结尾", "A GitHub link ending in /SKILL.md")
                : t("搜索技能…", "Search skills…")}
              onChange={(event) => handleQueryChange(event.target.value)}
            />
            {query && (
              <IconButton label={t("清空搜索", "Clear search")} onClick={() => handleQueryChange("")}>
                <X size={12} />
              </IconButton>
            )}
          </div>
        </div>
        {githubUrlInvalid && (
          <p className="skill-market__error" id={urlErrorId} role="alert">
            {t(
              "请粘贴受支持且直接指向 SKILL.md 文件的 GitHub 链接",
              "Paste a supported GitHub link that points directly at a SKILL.md file"
            )}
          </p>
        )}
        {!isGithub && failedSources.length > 0 && Boolean(results.length) && (
          <p className="skill-market__notice" role="status">
            {t("以下来源本次没有查通：{sources}", "These sources could not be reached: {sources}", {
              sources: failedSources.map((item) => SOURCE_LABELS[item]).join("、")
            })}
          </p>
        )}
        {installError && (
          <p className="skill-market__error" role="alert">{installError}</p>
        )}
        {!backendAvailable && (
          <p className="skill-market__notice" role="status">
            {t("浏览器预览无法搜索或安装在线技能。", "Searching and installing online skills is unavailable in browser preview.")}
          </p>
        )}

        <div className="skill-market__body">
          {!activeQuery.trim() ? (
            <div className="skill-market__state">
              <Search size={22} />
              <strong>{isGithub ? t("从 GitHub 安装", "Install from GitHub") : t("搜索技能", "Search skills")}</strong>
              <small>{isGithub
                ? t(
                  "粘贴某个技能 SKILL.md 文件的链接，例如 github.com/owner/repo/blob/main/skills/my-skill/SKILL.md",
                  "Paste a link to a skill's SKILL.md file, e.g. github.com/owner/repo/blob/main/skills/my-skill/SKILL.md"
                )
                : t("搜索在线技能源，查找可安装的技能。", "Search the online skill registries for something to install.")}</small>
            </div>
          ) : busy ? (
            <div className="skill-market__state">
              <LoaderCircle className="spin" size={22} />
              <strong>{t("搜索中…", "Searching…")}</strong>
            </div>
          ) : searchFailed ? (
            <div className="skill-market__state" role="alert">
              <X size={22} />
              <strong>{t("搜索失败", "Search failed")}</strong>
              <small>{t("搜索失败，请稍后重试。", "The search failed. Try again later.")}</small>
            </div>
          ) : !visible.length ? (
            <div className="skill-market__state">
              <Search size={22} />
              <strong>{t("未找到技能", "No skills found")}</strong>
              <small>{t("请尝试其他关键词，或本地导入 ZIP 文件 / 目录。", "Try another keyword, or import a ZIP file or directory locally.")}</small>
            </div>
          ) : (
            <ul className="skill-market__list">
              {visible.map((result) => {
                const done = installed.has(result.installSource);
                const busyRow = installing === result.installSource;
                return (
                  <li className="skill-market__row" key={`${result.sourceRegistry}:${result.slug}`}>
                    <div className="skill-market__copy">
                      <span className="skill-market__name">
                        <strong>{result.name}</strong>
                        {result.sourceUrl && (
                          <a
                            href={result.sourceUrl}
                            target="_blank"
                            rel="noreferrer noopener"
                            aria-label={t("查看 {name} 的源码", "View the source of {name}", { name: result.name })}
                          ><ExternalLink size={12} /></a>
                        )}
                      </span>
                      {result.description && <small>{result.description}</small>}
                      <span className="skill-market__meta">
                        {result.author && <em>{result.author}</em>}
                        {result.stars > 0 && (
                          <em><Star size={11} aria-hidden="true" />{result.stars}</em>
                        )}
                        {result.downloads > 0 && (
                          <em><Download size={11} aria-hidden="true" />{result.downloads}</em>
                        )}
                      </span>
                    </div>
                    <button
                      type="button"
                      className="button button--secondary button--small"
                      disabled={!backendAvailable || done || Boolean(installing)}
                      onClick={() => void install(result)}
                    >
                      {busyRow ? <LoaderCircle className="spin" size={13} /> : done ? <Check size={13} /> : <Download size={13} />}
                      {done ? t("已安装", "Installed") : t("安装", "Install")}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </div>
    </Dialog>
  );
}

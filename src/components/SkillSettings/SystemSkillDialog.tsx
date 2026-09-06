import { Check, FolderSearch, Import, LoaderCircle, Search, TriangleAlert, X } from "lucide-react";
import type { JSX } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import { hasBackendRuntime } from "../../lib/backend";
import { installSkillFromDirectory, scanSystemSkills } from "../../lib/runtime";
import type { SkillRecord, SystemSkillCandidate } from "../../types";
import { Dialog, IconButton } from "../Common";

/** Import skills from directories used by other coding tools on this machine. */

type ScanStatus = "loading" | "ready" | "error";

export function SystemSkillDialog({
  onClose,
  onInstalled
}: {
  onClose: () => void;
  onInstalled: (skill: SkillRecord) => void;
}): JSX.Element {
  const { t } = useI18n();
  const backendAvailable = hasBackendRuntime();
  const [status, setStatus] = useState<ScanStatus>("loading");
  const [candidates, setCandidates] = useState<SystemSkillCandidate[]>([]);
  const [query, setQuery] = useState("");
  const [importing, setImporting] = useState<string | null>(null);
  const [failedPath, setFailedPath] = useState<string | null>(null);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const scan = async () => {
    if (!backendAvailable) {
      setStatus("error");
      return;
    }
    setStatus("loading");
    setFailedPath(null);
    try {
      const found = await scanSystemSkills();
      if (!mountedRef.current) return;
      setCandidates(found);
      setStatus("ready");
    } catch {
      if (!mountedRef.current) return;
      setStatus("error");
    }
  };

  // biome-ignore lint/correctness/useExhaustiveDependencies: Scan once on open; only the Scan again button reruns it.
  useEffect(() => {
    void scan();
  }, []);

  const needle = query.trim().toLowerCase();
  const visible = useMemo(() => candidates.filter((candidate) => !needle
    || candidate.name.toLowerCase().includes(needle)
    || candidate.description.toLowerCase().includes(needle)
    || candidate.sourceName.toLowerCase().includes(needle)
    || candidate.directoryPath.toLowerCase().includes(needle)), [candidates, needle]);

  const importCandidate = async (candidate: SystemSkillCandidate) => {
    if (!backendAvailable || candidate.conflict || importing) return;
    setImporting(candidate.directoryPath);
    setFailedPath(null);
    try {
      const skill = await installSkillFromDirectory(candidate.directoryPath);
      if (!mountedRef.current) return;
      if (skill) {
        onInstalled(skill);
        setCandidates((current) => current.filter((item) => item.directoryPath !== candidate.directoryPath));
      }
    } catch {
      if (mountedRef.current) setFailedPath(candidate.directoryPath);
    } finally {
      if (mountedRef.current) setImporting(null);
    }
  };

  return (
    <Dialog
      title={t("系统 Skill", "System skills")}
      description={t("导入这台机器上其他工具里已经安装的技能", "Import skills already installed by other tools on this machine")}
      width="760px"
      onClose={onClose}
    >
      <div className="skill-system">
        <div className="skill-system__search">
          <Search size={14} aria-hidden="true" />
          <input
            value={query}
            aria-label={t("搜索系统 Skill", "Search system skills")}
            placeholder={t("搜索名称、来源或路径…", "Search by name, source, or path…")}
            onChange={(event) => setQuery(event.target.value)}
          />
          {query && (
            <IconButton label={t("清空搜索", "Clear search")} onClick={() => setQuery("")}>
              <X size={12} />
            </IconButton>
          )}
        </div>

        <div className="skill-system__body">
          {status === "loading" && (
            <div className="skill-market__state">
              <LoaderCircle className="spin" size={22} />
              <strong>{t("正在扫描…", "Scanning…")}</strong>
            </div>
          )}
          {status === "error" && (
            <div className="skill-market__state" role="alert">
              <TriangleAlert size={22} />
              <strong>{t("扫描失败", "Scan failed")}</strong>
              <small>{t("无法读取系统技能目录，请稍后重试。", "System skill locations could not be read. Try again later.")}</small>
              <button type="button" className="button button--secondary button--small" onClick={() => void scan()}>
                {t("重新扫描", "Scan again")}
              </button>
            </div>
          )}
          {status === "ready" && !candidates.length && (
            <div className="skill-market__state">
              <FolderSearch size={22} />
              <strong>{t("没有可导入的 Skill", "Nothing to import")}</strong>
              <small>{t("未在这台设备的其他编程工具中找到可导入的 Skill。", "No importable skills were found in other coding tools on this device.")}</small>
            </div>
          )}
          {status === "ready" && Boolean(candidates.length) && !visible.length && (
            <div className="skill-market__state">
              <Search size={22} />
              <strong>{t("没有匹配结果", "No results")}</strong>
            </div>
          )}
          {status === "ready" && visible.length > 0 && (
            <ul className="skill-system__list">
              {visible.map((candidate) => (
                <li className="skill-system__row" key={`${candidate.sourceName}:${candidate.directoryPath}`}>
                  <span className="skill-system__icon">
                    {candidate.conflict ? <TriangleAlert size={16} /> : <FolderSearch size={16} />}
                  </span>
                  <div className="skill-system__copy">
                    <span>
                      <strong>{candidate.name}</strong>
                      <em>{candidate.sourceName}</em>
                    </span>
                    {candidate.description && <small>{candidate.description}</small>}
                    <code>{candidate.directoryPath}</code>
                    {failedPath === candidate.directoryPath && (
                      <span className="skill-system__error" role="alert">
                        {t("导入失败，请重试。", "Import failed. Try again.")}
                      </span>
                    )}
                  </div>
                  <button
                    type="button"
                    className="button button--secondary button--small"
                    disabled={candidate.conflict || Boolean(importing)}
                    onClick={() => void importCandidate(candidate)}
                  >
                    {importing === candidate.directoryPath
                      ? <LoaderCircle className="spin" size={13} />
                      : candidate.conflict ? <Check size={13} /> : <Import size={13} />}
                    {candidate.conflict ? t("名称冲突", "Name conflict") : t("导入", "Import")}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </Dialog>
  );
}

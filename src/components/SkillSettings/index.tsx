import {
  ChevronDown,
  FolderSearch,
  Import,
  LoaderCircle,
  Plus,
  Search,
  Wrench,
  Trash2,
  X
} from "lucide-react";
import type { JSX } from "react";
import { useEffect, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import { hasBackendRuntime } from "../../lib/backend";
import { uninstallSkill } from "../../lib/runtime";
import type { SkillRecord } from "../../types";
import { Dialog, EmptyState, IconButton, Switch } from "../Common";
import { ImportSkillDialog } from "./ImportSkillDialog";
import { SkillDetailDialog } from "./SkillDetailDialog";
import { SkillMarketplaceDialog } from "./SkillMarketplaceDialog";
import { SystemSkillDialog } from "./SystemSkillDialog";
import "./SkillSettings.css";

/**
 * Skill settings page.
 *
 * The Add skill menu separates online search, system search, and local import
 * because each uses a distinct source.
 */

function upsertSkill(skills: SkillRecord[], installed: SkillRecord): SkillRecord[] {
  const index = skills.findIndex(
    (skill) => skill.folderName.localeCompare(installed.folderName, undefined, { sensitivity: "accent" }) === 0
  );
  if (index < 0) return [...skills, installed];
  return skills.map((skill, position) => (position === index ? installed : skill));
}

type OpenDialog = "marketplace" | "system" | "import" | null;

export function SkillSettings({
  skills,
  onChange
}: {
  skills: SkillRecord[];
  onChange: (skills: SkillRecord[]) => void;
}): JSX.Element {
  const { t } = useI18n();
  const backendAvailable = hasBackendRuntime();
  const skillsRef = useRef(skills);
  const addMenuRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [dialog, setDialog] = useState<OpenDialog>(null);
  const [detailId, setDetailId] = useState<string | null>(null);
  const [uninstallId, setUninstallId] = useState<string | null>(null);
  const [uninstallPending, setUninstallPending] = useState(false);
  const [uninstallFailed, setUninstallFailed] = useState(false);

  // Async installs and removals must write from the parent component's latest registry.
  // Props captured by a closure may be stale, and onChange has no functional form.
  skillsRef.current = skills;

  useEffect(() => {
    if (!searchOpen) return;
    searchInputRef.current?.focus();
  }, [searchOpen]);

  useEffect(() => {
    if (!addMenuOpen) return;
    const close = (event: MouseEvent) => {
      if (!addMenuRef.current?.contains(event.target as Node)) setAddMenuOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setAddMenuOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [addMenuOpen]);

  const needle = query.trim().toLocaleLowerCase();
  const visible = skills
    .filter((skill) => !needle
      || skill.name.toLocaleLowerCase().includes(needle)
      || skill.description.toLocaleLowerCase().includes(needle))
    .sort((left, right) => left.name.localeCompare(right.name, "zh"));
  const detail = skills.find((skill) => skill.id === detailId) ?? null;
  const uninstallTarget = skills.find((skill) => skill.id === uninstallId) ?? null;

  const handleInstalled = (installed: SkillRecord) => {
    onChange(upsertSkill(skillsRef.current, installed));
  };

  const confirmUninstall = async () => {
    if (!backendAvailable || !uninstallTarget || uninstallPending) return;
    setUninstallPending(true);
    setUninstallFailed(false);
    try {
      // Shrink the registry only after deleting the on-disk copy succeeds.
      await uninstallSkill(uninstallTarget.folderName);
      onChange(skillsRef.current.filter((skill) => skill.id !== uninstallTarget.id));
      if (detailId === uninstallTarget.id) setDetailId(null);
      setUninstallId(null);
    } catch {
      setUninstallFailed(true);
    } finally {
      setUninstallPending(false);
    }
  };

  return (
    <section className="settings-page skill-settings-page">
      <header className="skill-toolbar">
        <div className="skill-toolbar__heading">
          <h2 className="skill-toolbar__title">{t("技能", "Skills")}</h2>
          <div className={searchOpen ? "skill-search skill-search--open" : "skill-search"}>
            {searchOpen ? (
              <>
                <Search size={13} aria-hidden="true" />
                <input
                  ref={searchInputRef}
                  value={query}
                  aria-label={t("搜索技能", "Search skills")}
                  placeholder={t("搜索名称或描述…", "Search name or description…")}
                  onChange={(event) => setQuery(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key !== "Escape") return;
                    if (query) setQuery("");
                    else setSearchOpen(false);
                  }}
                />
                <IconButton
                  label={t("关闭搜索", "Close search")}
                  onClick={() => {
                    setQuery("");
                    setSearchOpen(false);
                  }}
                ><X size={12} /></IconButton>
              </>
            ) : (
              <IconButton label={t("搜索技能", "Search skills")} onClick={() => setSearchOpen(true)}>
                <Search size={14} />
              </IconButton>
            )}
          </div>
        </div>
        <div className="skill-add" ref={addMenuRef}>
          <button
            type="button"
            className="button button--small"
            aria-haspopup="menu"
            aria-expanded={addMenuOpen}
            onClick={() => setAddMenuOpen((current) => !current)}
          >
            <Plus size={13} />
            {t("添加技能", "Add skill")}
            <ChevronDown size={13} />
          </button>
          {addMenuOpen && (
            <div className="skill-add__menu" role="menu">
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setAddMenuOpen(false);
                  setDialog("marketplace");
                }}
              ><Search size={13} /><span>{t("在线搜索", "Search online")}</span></button>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setAddMenuOpen(false);
                  setDialog("system");
                }}
              ><FolderSearch size={13} /><span>{t("系统搜索", "Search this machine")}</span></button>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setAddMenuOpen(false);
                  setDialog("import");
                }}
              ><Import size={13} /><span>{t("本地导入", "Import locally")}</span></button>
            </div>
          )}
        </div>
      </header>

      <div className="skill-grid">
        {visible.map((skill) => (
          <div
            className={skill.enabled ? "skill-card" : "skill-card skill-card--disabled"}
            key={skill.id}
          >
            <button
              type="button"
              className="skill-card__main"
              aria-label={t("查看技能 {name}", "View skill {name}", { name: skill.name })}
              onClick={() => setDetailId(skill.id)}
            >
              <span className="skill-card__icon"><Wrench size={20} strokeWidth={1.5} /></span>
              <span className="skill-card__copy">
                <span className="skill-card__title">
                  <strong>{skill.name}</strong>
                  {skill.version.trim() && <em>{skill.version}</em>}
                  {!skill.enabled && <span>{t("已停用", "Disabled")}</span>}
                </span>
                <small>{skill.description || t("暂无描述", "No description")}</small>
              </span>
            </button>
            <div className="skill-card__actions">
              <Switch
                checked={skill.enabled}
                label={t("全局启用 {name}", "Enable {name} globally", { name: skill.name })}
                onChange={(enabled) => onChange(skills.map((item) => (
                  item.id === skill.id ? { ...item, enabled } : item
                )))}
              />
              <IconButton
                label={t("卸载 {name}", "Uninstall {name}", { name: skill.name })}
                className="icon-button--danger skill-card__remove"
                disabled={!backendAvailable}
                onClick={() => {
                  setUninstallFailed(false);
                  setUninstallId(skill.id);
                }}
              ><Trash2 size={14} /></IconButton>
            </div>
          </div>
        ))}

        {!skills.length && (
          <EmptyState
            icon={<Wrench size={22} />}
            title={t("还没有技能", "No skills yet")}
            description={t(
              "技能是一个带 SKILL.md 的目录。在线搜索一个，从这台机器上导入，或者装一个本地目录。",
              "A skill is a directory with a SKILL.md file. Search online, import one already on this machine, or install a local directory."
            )}
          />
        )}
        {Boolean(skills.length) && !visible.length && (
          <EmptyState
            icon={<Search size={22} />}
            title={t("没有匹配的技能", "No matching skills")}
            description={t("换一个关键词试试。", "Try another keyword.")}
          />
        )}
      </div>

      {dialog === "marketplace" && (
        <SkillMarketplaceDialog onClose={() => setDialog(null)} onInstalled={handleInstalled} />
      )}
      {dialog === "system" && (
        <SystemSkillDialog onClose={() => setDialog(null)} onInstalled={handleInstalled} />
      )}
      {dialog === "import" && (
        <ImportSkillDialog onClose={() => setDialog(null)} onInstalled={handleInstalled} />
      )}
      {detail && <SkillDetailDialog skill={detail} onClose={() => setDetailId(null)} />}

      {uninstallTarget && (
        <Dialog
          title={t("卸载技能", "Uninstall skill")}
          description={t("此操作无法撤销。", "This action cannot be undone.")}
          onClose={() => {
            if (!uninstallPending) setUninstallId(null);
          }}
          dismissible={!uninstallPending}
          width="480px"
          footer={(
            <>
              <button
                type="button"
                className="button button--secondary"
                disabled={uninstallPending}
                onClick={() => setUninstallId(null)}
              >{t("取消", "Cancel")}</button>
              <button
                type="button"
                className="button button--danger"
                disabled={uninstallPending}
                onClick={() => void confirmUninstall()}
              >
                {uninstallPending && <LoaderCircle className="spin" size={15} />}
                {uninstallPending ? t("卸载中…", "Uninstalling…") : t("卸载", "Uninstall")}
              </button>
            </>
          )}
        >
          <div className="skill-uninstall">
            <p>{t(
              "技能“{name}”的文件将从应用数据目录中移除。",
              "The files for “{name}” will be removed from the application data directory.",
              { name: uninstallTarget.name }
            )}</p>
            {uninstallFailed && (
              <p className="skill-uninstall__error" role="alert">
                {t("卸载失败，技能仍然保留。请重试。", "Uninstall failed and the skill was kept. Try again.")}
              </p>
            )}
          </div>
        </Dialog>
      )}
    </section>
  );
}

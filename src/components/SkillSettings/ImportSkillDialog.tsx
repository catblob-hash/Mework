import { Archive, CheckCircle2, CircleAlert, FolderOpen, Import, LoaderCircle } from "lucide-react";
import type { JSX } from "react";
import { useRef, useState } from "react";
import { useI18n } from "../../i18n";
import { hasBackendRuntime } from "../../lib/backend";
import { installSkillFromArchive, installSkillFromDirectory } from "../../lib/runtime";
import type { SkillRecord } from "../../types";
import { Dialog } from "../Common";
import { useFileDrop } from "./useFileDrop";

/**
 * Import local ZIP files or directories containing SKILL.md. Online search belongs in the sibling `SkillMarketplaceDialog`.
 *
 * Drag-and-drop uses Tauri's window event because webview HTML5 `File` objects have no installable path. Browser previews lack that event, so the drop area remains click-only.
 */

type ItemStatus = "pending" | "installing" | "success" | "error";
type ImportKind = "zip" | "directory";
interface ImportItem {
  id: string;
  kind: ImportKind;
  name: string;
  path: string;
  status: ItemStatus;
  skillName?: string;
  error?: string;
}

function nameFromPath(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

export function ImportSkillDialog({
  onClose,
  onInstalled
}: {
  onClose: () => void;
  onInstalled: (skill: SkillRecord) => void;
}): JSX.Element {
  const { t } = useI18n();
  const backendAvailable = hasBackendRuntime();
  const [items, setItems] = useState<ImportItem[]>([]);
  const [busy, setBusy] = useState<ImportKind | null>(null);
  const [banner, setBanner] = useState<string | null>(null);
  const dropRef = useRef<HTMLDivElement>(null);
  const mountedRef = useRef(true);

  const install = async (kind: ImportKind, path?: string): Promise<SkillRecord | null> => (
    kind === "zip" ? installSkillFromArchive(path) : installSkillFromDirectory(path)
  );

  const runQueue = async (queue: ImportItem[], kind: ImportKind) => {
    if (busy || !backendAvailable) return;
    setBusy(kind);
    setBanner(null);
    setItems(queue);
    let failed = 0;
    let succeeded = 0;
    try {
      for (const item of queue) {
        setItems((current) => current.map((entry) => (
          entry.id === item.id ? { ...entry, status: "installing", error: undefined } : entry
        )));
        try {
          const skill = await install(item.kind, item.path);
          if (!mountedRef.current) return;
          if (!skill) {
            failed += 1;
            setItems((current) => current.map((entry) => (
              entry.id === item.id
                ? { ...entry, status: "error", error: t("安装被取消", "Installation was cancelled") }
                : entry
            )));
            continue;
          }
          succeeded += 1;
          onInstalled(skill);
          setItems((current) => current.map((entry) => (
            entry.id === item.id ? { ...entry, status: "success", skillName: skill.name } : entry
          )));
        } catch (error) {
          if (!mountedRef.current) return;
          failed += 1;
          const message = error instanceof Error ? error.message : String(error);
          setItems((current) => current.map((entry) => (
            entry.id === item.id ? { ...entry, status: "error", error: message } : entry
          )));
        }
      }
      if (failed && queue.length > 1) {
        setBanner(t(
          "已安装 {success}/{total} 个技能，{failed} 个失败",
          "Installed {success}/{total} skills, {failed} failed",
          { success: succeeded, total: queue.length, failed }
        ));
      }
    } finally {
      if (mountedRef.current) setBusy(null);
    }
  };
  const pickAndInstall = (kind: ImportKind) => {
    const item: ImportItem = {
      id: `${kind}-picker`,
      kind,
      name: kind === "zip" ? t("从 ZIP 文件安装", "Install from ZIP") : t("从文件夹安装", "Install from a folder"),
      path: "",
      status: "pending"
    };
    void runQueue([item], kind);
  };

  const handleDroppedPaths = (paths: string[]) => {
    if (busy || !backendAvailable) return;
    // Only the `.zip` suffix selects the archive pipeline; every other path is
    // deliberately sent through the directory pipeline, and the host rejects
    // invalid non-directory/non-ZIP inputs explicitly.
    const queue: ImportItem[] = paths.map((path, index) => ({
      id: `${index}-${path}`,
      kind: path.toLowerCase().endsWith(".zip") ? "zip" : "directory",
      name: nameFromPath(path),
      path,
      status: "pending"
    }));
    if (!queue.length) return;
    void runQueue(queue, queue.some((item) => item.kind === "zip") ? "zip" : "directory");
  };

  const { over } = useFileDrop(dropRef, backendAvailable && !busy, handleDroppedPaths);

  return (
    <Dialog
      title={t("导入技能", "Import a skill")}
      description={t("从 ZIP 文件或目录安装技能", "Install a skill from a ZIP file or a directory")}
      width="560px"
      dismissible={!busy}
      onClose={() => {
        mountedRef.current = false;
        if (!busy) onClose();
      }}
    >
      <div className="skill-import">
        <div
          ref={dropRef}
          className={over ? "skill-import__drop skill-import__drop--over" : "skill-import__drop"}
        >
          <Import size={26} strokeWidth={1.2} aria-hidden="true" />
          <p>{t("把 ZIP 文件或目录拖到这里", "Drop a ZIP file or a directory here")}</p>
          <small>{t("支持 .zip 文件或包含 SKILL.md 的目录", "Accepts .zip files or directories containing SKILL.md")}</small>
        </div>

        <div className="skill-import__actions">
          <button
            type="button"
            className="button button--secondary button--small"
            disabled={!backendAvailable || Boolean(busy)}
            onClick={() => pickAndInstall("zip")}
          >
            {busy === "zip" ? <LoaderCircle className="spin" size={13} /> : <Archive size={13} />}
            {t("从 ZIP 文件安装", "Install from ZIP")}
          </button>
          <button
            type="button"
            className="button button--secondary button--small"
            disabled={!backendAvailable || Boolean(busy)}
            onClick={() => pickAndInstall("directory")}
          >
            {busy === "directory" ? <LoaderCircle className="spin" size={13} /> : <FolderOpen size={13} />}
            {t("从文件夹安装", "Install from a folder")}
          </button>
        </div>

        {items.length > 0 && (
          <div className="skill-import__results">
            {items.map((item) => (
              <div className="skill-import__result" key={item.id}>
                {item.status === "installing" && <LoaderCircle className="spin" size={13} />}
                {item.status === "success" && <CheckCircle2 size={13} className="skill-import__ok" />}
                {item.status === "error" && <CircleAlert size={13} className="skill-import__bad" />}
                {item.status === "pending" && <span className="skill-import__pending" aria-hidden="true" />}
                <span>
                  <strong>{item.skillName ?? item.name}</strong>
                  {item.status === "pending" && <small>{t("等待安装", "Queued")}</small>}
                  {item.status === "installing" && <small>{t("安装中…", "Installing…")}</small>}
                  {item.status === "error" && <small>{item.error}</small>}
                </span>
              </div>
            ))}
          </div>
        )}
        {banner && <p className="skill-import__banner" role="alert">{banner}</p>}
        {!backendAvailable && (
          <p className="skill-import__banner" role="status">
            {t("浏览器预览无法安装技能。", "Installing skills is unavailable in browser preview.")}
          </p>
        )}
      </div>
    </Dialog>
  );
}

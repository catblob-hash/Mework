import { useCallback, useEffect, useRef, useState } from "react";
import { CornerLeftUp, Folder, Loader2 } from "lucide-react";
import { Dialog } from "./Common";
import { useI18n } from "../i18n";
import { authorizeRemoteWorkspace, listRemoteDirectory } from "../lib/workspacePicker";
import type { RemoteDirectoryListing } from "../lib/workspacePicker";
import type { RunTarget } from "../types";

/** Where the browser starts when it has nowhere better: the remote user's home. */
const HOME = "~";

export interface RemoteDirectoryPickerProps {
  /** Machine to browse. Never `null` — the host machine has a native dialog. */
  machine: RunTarget;
  /** Machine name as the run-location picker spells it, used in the title. */
  machineName: string;
  /** Receives the path the host resolved and authorized. */
  onPick: (path: string) => void;
  onClose: () => void;
}

/**
 * Directory browser for a machine that is not this one.
 *
 * The host has a native folder dialog only for its own filesystem, so a
 * directory on a WSL distribution or an SSH machine is chosen here instead: each
 * level is read through that machine's own shell, and confirming records the
 * grant the way the native dialog does for a local path.
 *
 * The path field is editable on purpose. Browsing from the home directory to a
 * deeply nested project is a lot of round trips over SSH, and a user who already
 * knows the path should be able to type it; what makes the typed path safe is
 * that the host still resolves and verifies it before the grant exists.
 */
export function RemoteDirectoryPicker({
  machine,
  machineName,
  onPick,
  onClose
}: RemoteDirectoryPickerProps) {
  const { t } = useI18n();
  const [listing, setListing] = useState<RemoteDirectoryListing | null>(null);
  const [draft, setDraft] = useState(HOME);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Only the newest navigation may write state: a slow parent listing must not
  // land after the child the user already clicked into.
  const generation = useRef(0);

  const navigate = useCallback((path: string) => {
    const attempt = ++generation.current;
    setBusy(true);
    setError(null);
    void listRemoteDirectory(machine, path)
      .then((next) => {
        if (attempt !== generation.current) return;
        setListing(next);
        setDraft(next.path);
      })
      .catch((reason: unknown) => {
        if (attempt !== generation.current) return;
        setError(reason instanceof Error ? reason.message : String(reason));
      })
      .finally(() => {
        if (attempt === generation.current) setBusy(false);
      });
  }, [machine]);

  useEffect(() => navigate(HOME), [navigate]);

  const parentOf = (path: string): string => {
    const trimmed = path.replace(/\/+$/, "");
    const cut = trimmed.lastIndexOf("/");
    return cut <= 0 ? "/" : trimmed.slice(0, cut);
  };

  const join = (path: string, name: string): string => (
    path.endsWith("/") ? `${path}${name}` : `${path}/${name}`
  );

  const confirm = () => {
    const path = draft.trim();
    if (!path) {
      setError(t("请填写一个目录", "Enter a directory"));
      return;
    }
    setBusy(true);
    setError(null);
    void authorizeRemoteWorkspace(machine, path)
      .then(onPick)
      .catch((reason: unknown) => {
        setError(reason instanceof Error ? reason.message : String(reason));
        setBusy(false);
      });
  };

  return (
    <Dialog
      title={t("选择 {name} 上的工作区", "Choose a workspace on {name}", { name: machineName })}
      description={t(
        "目录经这台机器自己的 shell 读取；选中的目录会成为一个工作区。",
        "Each level is read through this machine's own shell. The directory you choose becomes a workspace."
      )}
      width="460px"
      onClose={onClose}
      footer={<>
        <button type="button" className="button button--secondary" onClick={onClose}>
          {t("取消", "Cancel")}
        </button>
        <button type="button" className="button button--primary" disabled={busy} onClick={confirm}>
          {t("选择", "Choose")}
        </button>
      </>}
    >
      <div className="remote-picker">
        <label className="run-location__field">
          <span>{t("路径", "Path")}</span>
          <input
            type="text"
            value={draft}
            spellCheck={false}
            onChange={(event) => {
              setDraft(event.target.value);
              setError(null);
            }}
            onKeyDown={(event) => {
              if (event.key !== "Enter") return;
              event.preventDefault();
              navigate(draft.trim() || HOME);
            }}
          />
        </label>
        <div className="remote-picker__list" role="listbox" aria-busy={busy || undefined}>
          {listing?.hasParent && (
            <button
              type="button"
              className="remote-picker__entry"
              disabled={busy}
              onClick={() => navigate(parentOf(listing.path))}
            >
              <CornerLeftUp size={13} />
              <span>{t("上一级", "Up one level")}</span>
            </button>
          )}
          {listing?.directories.map((name) => (
            <button
              key={name}
              type="button"
              className="remote-picker__entry"
              disabled={busy}
              onClick={() => navigate(join(listing.path, name))}
            >
              <Folder size={13} />
              <span className="remote-picker__name">{name}</span>
            </button>
          ))}
          {listing && listing.directories.length === 0 && (
            <p className="remote-picker__empty">
              {t("这个目录里没有子目录", "This directory has no subdirectories")}
            </p>
          )}
          {!listing && busy && (
            <p className="remote-picker__empty">
              <Loader2 size={13} className="remote-picker__spinner" />
              {t("正在读取…", "Reading…")}
            </p>
          )}
        </div>
      </div>
      {error && <p className="run-location__error" role="alert">{error}</p>}
    </Dialog>
  );
}

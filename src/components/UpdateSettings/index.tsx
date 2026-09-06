import {
  AlertTriangle,
  ArrowUpCircle,
  CheckCircle2,
  Download,
  ExternalLink,
  FolderOpen,
  RefreshCw,
  ShieldCheck,
  X
} from "lucide-react";
import type { JSX } from "react";
import { useEffect, useState, useSyncExternalStore } from "react";
import { useI18n } from "../../i18n";
import type { TranslationFunction } from "../../i18n";
import { appUpdateController, errorMessage } from "../../lib/appUpdateController";
import type { AppUpdateController, AppUpdateState } from "../../lib/appUpdateController";
import { hasBackendRuntime } from "../../lib/backend";
import { appVersionInfo } from "../../lib/runtime";
import type { AppVersionInfo } from "../../types";
import { IconButton } from "../Common";
import { MarkdownContent } from "../MarkdownContent";
import { MeworkIcon } from "../MeworkIcon";
import { SettingsPageHeading } from "../SettingsPageHeading";
import "./UpdateSettings.css";

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

function formatDate(iso: string, locale: string): string {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(date);
}

function formatDateTime(iso: string, locale: string): string {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeStyle: "short" }).format(date);
}

function flavorLabel(info: AppVersionInfo, t: TranslationFunction): string {
  return info.flavor === "installer" ? t("安装版", "Installer") : t("便携版", "Portable");
}

function VersionCard({
  info,
  infoError,
  connected
}: {
  info: AppVersionInfo | null;
  infoError: string;
  connected: boolean;
}) {
  const { t } = useI18n();
  return (
    <article className="update-settings__card update-settings__version">
      <div className="update-settings__mark" aria-hidden="true">
        <MeworkIcon size={28} />
      </div>
      <div className="update-settings__version-copy">
        <div className="update-settings__version-line">
          <strong>Mework</strong>
          <span className="update-settings__version-number" data-testid="current-version">
            {info ? t("v{version}", "v{version}", { version: info.version }) : "—"}
          </span>
        </div>
        {info && (
          <div className="update-settings__badges">
            <span className="update-settings__badge">{flavorLabel(info, t)}</span>
            <span className="update-settings__badge update-settings__badge--mono">{info.arch}</span>
            {info.developmentBuild && (
              <span className="update-settings__badge update-settings__badge--warning">
                {t("开发构建", "Development build")}
              </span>
            )}
          </div>
        )}
        {info && (
          <p className="update-settings__path" title={info.executableDir}>
            {info.executableDir}
          </p>
        )}
        {!connected && (
          <p className="update-settings__hint">
            {t(
              "当前预览没有连接应用后端，无法读取版本信息或检查更新。",
              "This preview is not connected to the app backend, so the version cannot be read and updates cannot be checked."
            )}
          </p>
        )}
        {connected && infoError && (
          <p className="update-settings__hint update-settings__hint--error" role="alert">
            {infoError}
          </p>
        )}
      </div>
      {info && (
        <div className="update-settings__links">
          <a href={info.repositoryUrl} target="_blank" rel="noreferrer noopener">
            <ExternalLink size={11} />
            {t("源代码", "Source")}
          </a>
          <a href={info.releasesUrl} target="_blank" rel="noreferrer noopener">
            <ExternalLink size={11} />
            {t("全部发布", "All releases")}
          </a>
        </div>
      )}
    </article>
  );
}

function statusOf(
  state: AppUpdateState,
  info: AppVersionInfo | null,
  locale: string,
  t: TranslationFunction
): { tone: "neutral" | "success" | "accent" | "danger"; title: string; detail: string } {
  switch (state.phase) {
    case "idle":
    case "checking":
      return {
        tone: "neutral",
        title: t("正在检查更新…", "Checking for updates…"),
        detail: t("正在向 GitHub Releases 查询最新版本。", "Asking GitHub Releases for the latest version.")
      };
    case "check_failed":
      return {
        tone: "danger",
        title: t("检查更新失败", "Update check failed"),
        detail: state.message
      };
    case "checked":
    case "downloading":
    case "download_failed":
    case "downloaded":
    case "installing":
    case "install_failed": {
      const { check } = state;
      if (!check.updateAvailable) {
        return {
          tone: "success",
          title: t("已是最新版本", "You are up to date"),
          detail: t(
            "v{version} 就是最新发布；检查于 {time}。",
            "v{version} is the latest release; checked {time}.",
            { version: check.currentVersion, time: formatDateTime(check.checkedAt, locale) }
          )
        };
      }
      const published = check.release.publishedAt
        ? t("，发布于 {date}", ", published {date}", { date: formatDate(check.release.publishedAt, locale) })
        : "";
      const flavorNote = info && !check.asset
        ? t(
          "这次发布没有提供{flavor}的安装文件，请到发布页手动下载。",
          "This release has no file for the {flavor} flavor; download it from the release page.",
          { flavor: flavorLabel(info, t) }
        )
        : "";
      return {
        tone: "accent",
        title: t("发现新版本 v{version}", "Version v{version} is available", { version: check.latestVersion }),
        detail: t(
          "当前 v{current}{published}。{flavorNote}",
          "You have v{current}{published}. {flavorNote}",
          { current: check.currentVersion, published, flavorNote }
        ).trim()
      };
    }
  }
}

function UpdateCard({
  state,
  info,
  controller
}: {
  state: AppUpdateState;
  info: AppVersionInfo | null;
  controller: AppUpdateController;
}) {
  const { resolvedLanguage, t } = useI18n();
  const status = statusOf(state, info, resolvedLanguage, t);
  const checking = state.phase === "idle" || state.phase === "checking";
  const check = state.phase === "idle" || state.phase === "checking" || state.phase === "check_failed"
    ? null
    : state.check;
  const installer = info?.flavor === "installer";
  const percent = state.phase === "downloading" && state.totalBytes > 0
    ? Math.min(100, Math.round((state.receivedBytes / state.totalBytes) * 100))
    : 0;

  return (
    <article className={`update-settings__card update-settings__update update-settings__update--${status.tone}`}>
      <div className="update-settings__status">
        <span className="update-settings__status-icon" aria-hidden="true">
          {status.tone === "success" && <CheckCircle2 size={18} />}
          {status.tone === "accent" && <ArrowUpCircle size={18} />}
          {status.tone === "danger" && <AlertTriangle size={18} />}
          {status.tone === "neutral" && <RefreshCw size={18} className={checking ? "spin" : undefined} />}
        </span>
        <div className="update-settings__status-copy" role={status.tone === "danger" ? "alert" : undefined}>
          <strong>{status.title}</strong>
          <span>{status.detail}</span>
        </div>
        <div className="update-settings__actions">
          {state.phase === "check_failed" && (
            <button
              type="button"
              className="button button--secondary button--small"
              onClick={() => void controller.check({ force: true })}
            >
              {t("重试", "Retry")}
            </button>
          )}
          {check?.updateAvailable && check.asset && (state.phase === "checked" || state.phase === "download_failed") && (
            <button
              type="button"
              className="button button--primary button--small"
              onClick={() => void controller.download()}
            >
              <Download size={14} />
              {t("下载更新（{size}）", "Download update ({size})", { size: formatBytes(check.asset.size) })}
            </button>
          )}
          {check?.updateAvailable && !check.asset && (
            <a
              className="button button--primary button--small"
              href={check.release.htmlUrl}
              target="_blank"
              rel="noreferrer noopener"
            >
              <ExternalLink size={14} />
              {t("前往发布页", "Open release page")}
            </a>
          )}
          {(state.phase === "downloaded" || state.phase === "install_failed") && (
            <button
              type="button"
              className="button button--primary button--small"
              onClick={() => void controller.install()}
            >
              {installer ? <ArrowUpCircle size={14} /> : <FolderOpen size={14} />}
              {installer ? t("安装并重启", "Install and restart") : t("在文件夹中显示", "Show in folder")}
            </button>
          )}
        </div>
      </div>

      {state.phase === "downloading" && (
        <div className="update-settings__progress-row">
          <div
            className="update-settings__progress"
            role="progressbar"
            aria-label={t("下载进度", "Download progress")}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={percent}
          >
            <span style={{ width: `${percent}%` }} />
          </div>
          <span className="update-settings__progress-text">
            {state.verifying
              ? t("正在校验…", "Verifying…")
              : `${formatBytes(state.receivedBytes)} / ${formatBytes(state.totalBytes)} · ${percent}%`}
          </span>
          <IconButton
            label={t("取消下载", "Cancel download")}
            disabled={state.verifying}
            onClick={() => void controller.cancelDownload()}
          >
            <X size={14} />
          </IconButton>
        </div>
      )}

      {state.phase === "download_failed" && (
        <p className="update-settings__note update-settings__note--error" role="alert">
          <AlertTriangle size={12} />
          {t("下载失败：{message}", "Download failed: {message}", { message: state.message })}
        </p>
      )}

      {(state.phase === "downloaded" || state.phase === "installing" || state.phase === "install_failed") && (
        <div className="update-settings__ready">
          <p className="update-settings__note">
            <ShieldCheck size={12} />
            {state.download.verification === "verified"
              ? t("已下载 {file}（{size}），SHA-256 与发布的校验和一致。", "Downloaded {file} ({size}); SHA-256 matches the published checksum.", {
                file: state.download.fileName,
                size: formatBytes(state.download.sizeBytes)
              })
              : t("已下载 {file}（{size}）。这次发布没有提供校验和，完整性由 HTTPS 保证。", "Downloaded {file} ({size}). This release publishes no checksum; HTTPS is the integrity check.", {
                file: state.download.fileName,
                size: formatBytes(state.download.sizeBytes)
              })}
          </p>
          <p className="update-settings__note">
            {installer
              ? t(
                "点击「安装并重启」会启动安装程序并关闭 Mework；设置与数据保留，安装完成后自动重新打开。",
                "“Install and restart” launches the installer and closes Mework; settings and data are kept, and the app reopens when it finishes."
              )
              : t(
                "便携版需要手动替换：关闭 Mework，把压缩包解压到 {dir} 覆盖旧文件，再重新打开。",
                "The portable flavor is replaced by hand: close Mework, unzip the archive over {dir}, then reopen it.",
                { dir: info?.executableDir ?? "" }
              )}
          </p>
          {state.phase === "installing" && (
            <p className="update-settings__note">
              <RefreshCw size={12} className="spin" />
              {installer
                ? t("正在启动安装程序…", "Starting the installer…")
                : t("正在打开文件夹…", "Opening the folder…")}
            </p>
          )}
          {state.phase === "install_failed" && (
            <p className="update-settings__note update-settings__note--error" role="alert">
              <AlertTriangle size={12} />
              {state.message}
            </p>
          )}
        </div>
      )}

      {check?.updateAvailable && (
        <section className="update-settings__notes" aria-label={t("发布说明", "Release notes")}>
          <header>
            <strong>{check.release.name}</strong>
            <a href={check.release.htmlUrl} target="_blank" rel="noreferrer noopener">
              <ExternalLink size={11} />
              {t("在 GitHub 上查看", "View on GitHub")}
            </a>
          </header>
          {check.release.notes.trim()
            ? <MarkdownContent content={check.release.notes} className="update-settings__notes-body" />
            : <p className="update-settings__hint">{t("这次发布没有填写说明。", "This release has no notes.")}</p>}
        </section>
      )}
    </article>
  );
}

export function UpdateSettings({
  controller = appUpdateController
}: {
  controller?: AppUpdateController;
}): JSX.Element {
  const { t } = useI18n();
  const connected = hasBackendRuntime();
  const state = useSyncExternalStore(controller.subscribe, controller.current);
  const [info, setInfo] = useState<AppVersionInfo | null>(null);
  const [infoError, setInfoError] = useState("");
  const checking = state.phase === "idle" || state.phase === "checking";
  const busy = state.phase === "downloading" || state.phase === "installing";

  useEffect(() => {
    if (!connected) return;
    let cancelled = false;
    appVersionInfo()
      .then((next) => {
        if (!cancelled) setInfo(next);
      })
      .catch((error: unknown) => {
        if (!cancelled) setInfoError(errorMessage(error));
      });
    void controller.check();
    return () => {
      cancelled = true;
    };
  }, [connected, controller]);

  return (
    <section className="settings-page update-settings">
      <SettingsPageHeading
        title={t("版本更新", "Updates")}
        description={t(
          "从 GitHub Releases 检查新版本。安装版可以在应用内下载并安装；便携版下载压缩包后由你手动替换。",
          "Check GitHub Releases for a newer version. The installer flavor downloads and installs in place; the portable flavor downloads the archive for you to unpack."
        )}
        action={connected ? (
          <div className="settings-page-heading__actions">
            <button
              type="button"
              className="button button--secondary button--small"
              disabled={checking || busy}
              aria-busy={checking}
              onClick={() => void controller.check({ force: true })}
            >
              <RefreshCw className={checking ? "spin" : undefined} size={14} />
              {t("检查更新", "Check for updates")}
            </button>
          </div>
        ) : undefined}
      />

      <VersionCard info={info} infoError={infoError} connected={connected} />
      {connected && <UpdateCard state={state} info={info} controller={controller} />}
    </section>
  );
}

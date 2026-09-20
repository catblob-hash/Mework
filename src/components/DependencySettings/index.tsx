import {
  AlertTriangle,
  ExternalLink,
  FolderOpen,
  Package,
  Plus,
  RefreshCw,
  Trash2
} from "lucide-react";
import type { JSX } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import { hasBackendRuntime } from "../../lib/backend";
import {
  environmentToolSnapshots,
  revealEnvironmentTool
} from "../../lib/runtime";
import type {
  EnvironmentToolDefinition,
  EnvironmentToolSnapshot
} from "../../types";
import { Dialog, Field, IconButton } from "../Common";
import { SettingsPageHeading } from "../SettingsPageHeading";
import "./DependencySettings.css";

const TOOL_NAME_PATTERN = /^[a-zA-Z][a-zA-Z0-9_-]*$/;
const SKELETON_KEYS = ["first", "second", "third", "fourth"] as const;

function compactUrlLabel(value: string): string {
  try {
    const url = new URL(value);
    if (url.hostname.toLowerCase() === "github.com") {
      return url.pathname.replace(/^\/+|\/+$/g, "") || url.hostname;
    }
    return url.hostname;
  } catch {
    const withoutScheme = value.replace(/^https?:\/\//i, "");
    if (/^github\.com\//i.test(withoutScheme)) return withoutScheme.replace(/^github\.com\//i, "");
    return withoutScheme.split("/", 1)[0] ?? value;
  }
}

function AddToolDialog({
  knownNames,
  onAdd,
  onClose
}: {
  knownNames: ReadonlySet<string>;
  onAdd: (tool: EnvironmentToolDefinition) => void;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState("");
  const [executable, setExecutable] = useState("");
  const [versionArguments, setVersionArguments] = useState("");
  const [nameTouched, setNameTouched] = useState(false);
  const normalizedName = name.trim().toLowerCase();
  const nameMissing = name.trim() === "";
  const nameInvalid = !nameMissing && !TOOL_NAME_PATTERN.test(name.trim());
  const nameDuplicate = !nameMissing && knownNames.has(normalizedName);
  const nameError = nameMissing
    ? t("请输入名称。", "Enter a name.")
    : nameInvalid
      ? t("名称必须以字母开头，并且只能包含字母、数字、下划线或连字符。", "The name must start with a letter and contain only letters, numbers, underscores, or hyphens.")
      : nameDuplicate
        ? t("已有同名工具。", "A tool with this name already exists.")
        : "";
  const valid = !nameError && executable.trim() !== "";

  const submit = () => {
    setNameTouched(true);
    if (!valid) return;
    onAdd({
      name: name.trim(),
      executable: executable.trim(),
      versionArgs: versionArguments.trim() === ""
        ? []
        : versionArguments.trim().split(/\s+/)
    });
  };

  return (
    <Dialog
      title={t("添加工具", "Add tool")}
      description={t("添加一个要在 PATH 上检测的命令行工具。", "Add a command-line tool to detect on PATH.")}
      onClose={onClose}
      width="480px"
      footer={(
        <>
          <button type="button" className="button button--secondary" onClick={onClose}>
            {t("取消", "Cancel")}
          </button>
          <button type="button" className="button button--primary" disabled={!valid} onClick={submit}>
            {t("添加工具", "Add tool")}
          </button>
        </>
      )}
    >
      <form
        className="dependency-settings__dialog-fields"
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
      >
        <Field label={t("名称", "Name")}>
          <input
            aria-label={t("名称", "Name")}
            className={`input${nameTouched && nameError ? " input--error" : ""}`}
            value={name}
            onBlur={() => setNameTouched(true)}
            onChange={(event) => {
              setName(event.target.value);
              if (nameTouched || event.target.value !== "") setNameTouched(true);
            }}
            aria-invalid={nameTouched && Boolean(nameError)}
            aria-describedby={nameTouched && nameError ? "dependency-tool-name-error" : undefined}
          />
          {nameTouched && nameError && (
            <span id="dependency-tool-name-error" className="dependency-settings__field-error" role="alert">
              {nameError}
            </span>
          )}
        </Field>
        <Field label={t("可执行文件名", "Executable name")}>
          <input
            aria-label={t("可执行文件名", "Executable name")}
            className="input"
            value={executable}
            onChange={(event) => setExecutable(event.target.value)}
          />
        </Field>
        <Field
          label={t("版本参数", "Version arguments")}
          hint={t("多个参数请用空格分隔；留空时使用 --version。", "Separate arguments with spaces; leave blank to use --version.")}
        >
          <input
            aria-label={t("版本参数", "Version arguments")}
            className="input"
            value={versionArguments}
            placeholder={t("--version", "--version")}
            onChange={(event) => setVersionArguments(event.target.value)}
          />
        </Field>
      </form>
    </Dialog>
  );
}

function ToolCard({
  snapshot,
  onDelete
}: {
  snapshot: EnvironmentToolSnapshot;
  onDelete: () => void;
}) {
  const { t } = useI18n();
  const installed = snapshot.path !== "";
  const links = [
    { kind: "repo", value: snapshot.repoUrl },
    { kind: "homepage", value: snapshot.homepage }
  ].filter((entry) => entry.value !== "");

  const reveal = () => {
    if (!hasBackendRuntime()) return;
    void revealEnvironmentTool(snapshot.executable).catch((error: unknown) => {
      console.error("Failed to reveal environment tool", error);
    });
  };

  return (
    <article className={`dependency-settings__card${installed ? " dependency-settings__card--installed" : ""}`}>
      <div className={`dependency-settings__icon${installed ? " dependency-settings__icon--installed" : ""}`} aria-hidden="true">
        <Package size={20} />
      </div>
      <div className="dependency-settings__card-copy">
        <div className="dependency-settings__identity">
          <strong>{snapshot.name}</strong>
          <small>{t("（{executable}）", "({executable})", { executable: snapshot.executable })}</small>
        </div>
        {installed && (
          <div className="dependency-settings__badges">
            {snapshot.version && (
              <span className="dependency-settings__badge">
                {t("v{version}", "v{version}", { version: snapshot.version })}
              </span>
            )}
            <span className="dependency-settings__badge dependency-settings__badge--system" title={snapshot.path}>
              {t("系统", "System")}
            </span>
            {snapshot.error && (
              <span className="dependency-settings__badge dependency-settings__badge--error" title={snapshot.error}>
                <AlertTriangle size={10} />
                {t("版本检测失败", "Version check failed")}
              </span>
            )}
          </div>
        )}
        <p>{snapshot.description || t("没有描述。", "No description.")}</p>
      </div>
      {!snapshot.builtin && (
        <IconButton
          className="icon-button--danger dependency-settings__delete"
          label={t("删除 {name}", "Delete {name}", { name: snapshot.name })}
          onClick={onDelete}
        >
          <Trash2 size={15} />
        </IconButton>
      )}
      <div className="dependency-settings__footer">
        <div className="dependency-settings__links">
          {links.map((link) => (
            <a
              key={link.kind}
              href={link.value}
              target="_blank"
              rel="noreferrer noopener"
              title={link.kind === "repo"
                ? t("打开 {name} 的代码仓库", "Open the {name} repository", { name: snapshot.name })
                : t("打开 {name} 的主页", "Open the {name} homepage", { name: snapshot.name })}
            >
              <ExternalLink size={11} />
              {compactUrlLabel(link.value)}
            </a>
          ))}
        </div>
        {installed && (
          <button type="button" className="dependency-settings__reveal" onClick={reveal}>
            <FolderOpen size={12} />
            {t("打开所在目录", "Open containing folder")}
          </button>
        )}
      </div>
      {!installed && (
        <div className="dependency-settings__missing">
          <strong>{t("未检测到", "Not detected")}</strong>
          <span>{t("请自行安装，然后重启应用再检测。", "Install it yourself, then restart the app and detect again.")}</span>
        </div>
      )}
    </article>
  );
}

function SkeletonCard() {
  return (
    <div className="dependency-settings__card dependency-settings__skeleton" aria-hidden="true">
      <span className="dependency-settings__skeleton-icon" />
      <div>
        <span className="dependency-settings__skeleton-line dependency-settings__skeleton-line--title" />
        <span className="dependency-settings__skeleton-line" />
        <span className="dependency-settings__skeleton-line dependency-settings__skeleton-line--short" />
      </div>
    </div>
  );
}

export function DependencySettings({
  tools,
  onChange
}: {
  tools: EnvironmentToolDefinition[];
  onChange: (tools: EnvironmentToolDefinition[]) => void;
}): JSX.Element {
  const { t } = useI18n();
  const [snapshots, setSnapshots] = useState<EnvironmentToolSnapshot[] | null>(null);
  const [probing, setProbing] = useState(true);
  const [probeError, setProbeError] = useState("");
  const [addOpen, setAddOpen] = useState(false);
  const requestSequence = useRef(0);
  // Depend on serialized fields because parent saves often rebuild the array;
  // depending on tools directly would re-probe after an unchanged parent render.
  const toolsSignature = JSON.stringify(tools.map((tool) => [tool.name, tool.executable, tool.versionArgs]));

  const probe = useCallback(async () => {
    const request = ++requestSequence.current;
    setProbing(true);
    setProbeError("");
    if (!hasBackendRuntime()) {
      setSnapshots([]);
      setProbing(false);
      setProbeError(t("当前预览没有连接应用后端，无法检测环境依赖。", "This preview is not connected to the app backend, so dependencies cannot be detected."));
      return;
    }
    try {
      const next = await environmentToolSnapshots();
      if (request !== requestSequence.current) return;
      setSnapshots(next);
    } catch {
      if (request !== requestSequence.current) return;
      setProbeError(t("环境依赖检测失败。", "Dependency detection failed."));
    } finally {
      if (request === requestSequence.current) setProbing(false);
    }
  }, [t]);

  useEffect(() => {
    // Read the signature so the dependency checker observes it; probe does not
    // need to know which field changed.
    void toolsSignature;
    void probe();
  }, [probe, toolsSignature]);

  const knownNames = useMemo(() => {
    const names = new Set(tools.map((tool) => tool.name.trim().toLowerCase()));
    for (const snapshot of snapshots ?? []) names.add(snapshot.name.trim().toLowerCase());
    return names;
  }, [snapshots, tools]);
  const totalCount = snapshots?.length ?? tools.length;
  const headingTitle = (
    <>
      {t("环境依赖", "Dependencies")}
      <span className="dependency-settings__count">{totalCount}</span>
    </>
  );

  const removeTool = (snapshot: EnvironmentToolSnapshot) => {
    const target = snapshot.name.trim().toLowerCase();
    onChange(tools.filter((tool) => tool.name.trim().toLowerCase() !== target));
  };

  return (
    <section className="settings-page dependency-settings">
      <SettingsPageHeading
        title={headingTitle}
        description={t("检测这些命令行工具是否在 PATH 上。本页只做检测，不会下载或安装任何东西。", "Check whether these command-line tools are on PATH. This page only detects them and never downloads or installs anything.")}
        action={(
          <div className="settings-page-heading__actions">
            <IconButton
              label={t("刷新", "Refresh")}
              disabled={probing}
              aria-busy={probing}
              onClick={() => void probe()}
            >
              <RefreshCw className={probing ? "spin" : undefined} size={15} />
            </IconButton>
            <button type="button" className="button button--primary button--small" onClick={() => setAddOpen(true)}>
              <Plus size={14} />
              {t("添加工具", "Add tool")}
            </button>
          </div>
        )}
      />

      {probeError && (
        <div className="dependency-settings__probe-error" role="alert">
          <AlertTriangle size={15} />
          <span>{probeError}</span>
          <button type="button" className="button button--secondary button--small" onClick={() => void probe()}>
            {t("重试", "Retry")}
          </button>
        </div>
      )}

      {snapshots === null && probing
        ? (
          <div className="dependency-settings__grid">
            {SKELETON_KEYS.map((key) => <SkeletonCard key={key} />)}
          </div>
        )
        : (
          <div className="dependency-settings__grid">
            {(snapshots ?? []).map((snapshot) => (
              <ToolCard
                key={`${snapshot.builtin ? "builtin" : "custom"}-${snapshot.name}-${snapshot.executable}`}
                snapshot={snapshot}
                onDelete={() => removeTool(snapshot)}
              />
            ))}
          </div>
        )}

      {addOpen && (
        <AddToolDialog
          knownNames={knownNames}
          onClose={() => setAddOpen(false)}
          onAdd={(tool) => {
            onChange([...tools, tool]);
            setAddOpen(false);
            // Re-probe the new catalog without implying that the tool was installed.
            void probe();
          }}
        />
      )}
    </section>
  );
}

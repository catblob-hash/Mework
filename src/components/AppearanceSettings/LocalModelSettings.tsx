import { X } from "lucide-react";
import type { JSX } from "react";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { isMainlandChinaLocale, useI18n } from "../../i18n";
import { localModelController } from "../../lib/localModel";
import type {
  LocalModelMachine,
  LocalModelPhase,
  LocalModelPreferences,
  LocalModelPromptReport,
  LocalModelStatus,
  LocalModelUnavailable,
  LocalModelVariantId,
  LocalModelVariantStatus
} from "../../types";
import { Dialog, IconButton, Switch } from "../Common";
import { formatBytes } from "../FilePreview/format";
import { SettingRow, SettingsCard } from "./rows";

type Translate = ReturnType<typeof useI18n>["t"];
type Use = "titles" | "shellExplanations";

/** Quiet time after typing before a prompt's KV cache is recomputed. */
const PROMPT_SETTLE_MS = 800;

type LocalModelSettingsProps = {
  preferences: LocalModelPreferences;
  onChange: (update: (current: LocalModelPreferences) => LocalModelPreferences) => void;
};

function percentOf(done: number, total: number): number {
  return total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;
}

export function variantName(id: LocalModelVariantId, t: Translate): string {
  switch (id) {
    case "ane":
      return t("神经网络引擎版（Core ML）", "Neural Engine build (Core ML)");
    case "mlx":
      return t("GPU 版（MLX）", "GPU build (MLX)");
    case "llama":
      return t("llama.cpp 版", "llama.cpp build");
  }
}

function variantDescription(id: LocalModelVariantId, t: Translate): string {
  switch (id) {
    case "ane":
      return t(
        "在神经网络引擎上运行：省电，不占用显卡。第一次加载要为这台 Mac 编译，约两分钟，之后一秒内加载。",
        "Runs on the Neural Engine: low power, leaves the GPU alone. The first load compiles it for this Mac, about two minutes; after that it loads in a second."
      );
    case "mlx":
      return t(
        "用 MLX 在显卡上运行：加载快，但生成时会占用显卡。本机已有同版本的 MLX 时直接用它的显卡内核库，否则一并下载。",
        "Runs on the GPU with MLX: loads fast, but uses the GPU while it writes. Uses the GPU kernels of this Mac's own copy of the same MLX release if there is one, otherwise downloads them too."
      );
    case "llama":
      return t(
        "用 llama.cpp 运行 GGUF 版；有显卡时放在显卡上。",
        "Runs the GGUF build with llama.cpp, on the GPU when there is one."
      );
  }
}

export function unavailableReason(reason: LocalModelUnavailable, t: Translate): string {
  switch (reason) {
    case "needsAppleSilicon":
      return t("需要 Apple 芯片的 Mac", "Needs a Mac with Apple silicon");
    case "noNeuralEngine":
      return t("这台 Mac 上没有可用的神经网络引擎", "No usable Neural Engine on this Mac");
    case "needsMacos14":
      return t("需要 macOS 14 或更高版本", "Needs macOS 14 or later");
    case "needsMacos15":
      return t("需要 macOS 15 或更高版本", "Needs macOS 15 or later");
    case "notInThisBuild":
      return t("这个版本的 Mework 没有包含它", "Not included in this build of Mework");
  }
}

/** "Apple M4 Pro (Mac16,7) · 16-core Neural Engine · macOS 15.4", or null off a Mac. */
export function machineSummary(machine: LocalModelMachine, t: Translate): string | null {
  if (!machine.chip && !machine.osVersion) return null;
  const parts: string[] = [];
  if (machine.chip) {
    parts.push(machine.model ? t("{chip}（{model}）", "{chip} ({model})", { chip: machine.chip, model: machine.model }) : machine.chip);
  }
  parts.push(
    machine.neuralEngineCores
      ? t("{cores} 核神经网络引擎", "{cores}-core Neural Engine", { cores: machine.neuralEngineCores })
      : t("无神经网络引擎", "No Neural Engine")
  );
  if (machine.osVersion) parts.push(`macOS ${machine.osVersion}`);
  return parts.join(" · ");
}

function downloadSource(source: Extract<LocalModelPhase, { phase: "downloading" }>["source"], t: Translate): string {
  switch (source) {
    case "huggingFace":
      return "Hugging Face";
    case "hfMirror":
      return "hf-mirror.com";
    case "mirror":
      return t("镜像", "mirror");
  }
}

/** One build's state in a line. */
export function variantSummary(variant: LocalModelVariantStatus, status: LocalModelStatus, t: Translate): string {
  switch (variant.phase) {
    case "missing":
      return t("未下载 · 约 {size}", "Not downloaded · about {size}", { size: formatBytes(variant.downloadBytes) });
    case "unsupported":
      return unavailableReason(variant.reason, t);
    case "downloading":
      return t("正在下载 {percent}%（{source}）", "Downloading {percent}% ({source})", {
        percent: percentOf(variant.received, variant.total),
        source: downloadSource(variant.source, t)
      });
    case "preparing":
      switch (variant.step) {
        case "compile":
          return t("正在编译模型…", "Compiling the model…");
        case "verify":
          return t("正在校验编译结果…", "Checking the compiled model…");
      }
      return t("正在准备模型…", "Preparing the model…");
    case "ready": {
      if (status.active !== variant.id) {
        return t("已下载 · 占用 {size}", "Downloaded · {size} on disk", { size: formatBytes(variant.diskBytes) });
      }
      if (status.warming) {
        return variant.id === "ane"
          ? t(
            "正在加载并缓存提示词（第一次在这台 Mac 上加载要编译，约两分钟）…",
            "Loading and caching the prompts (the first load on this Mac compiles it, about two minutes)…"
          )
          : t("正在加载并缓存提示词…", "Loading and caching the prompts…");
      }
      const parts = [t("使用中", "In use")];
      if (status.device) parts.push(status.device);
      parts.push(t("占用 {size}", "{size} on disk", { size: formatBytes(variant.diskBytes) }));
      return parts.join(" · ");
    }
    case "failed":
      return t("安装失败：{message}", "Install failed: {message}", { message: variant.message });
  }
}

/** The card's summary: the build in use, or what is happening instead. */
export function statusSummary(status: LocalModelStatus | null, t: Translate): string {
  if (!status) return t("正在读取状态…", "Reading status…");
  const busy = status.variants.find((variant) => variant.phase === "downloading" || variant.phase === "preparing");
  if (busy) return `${variantName(busy.id, t)} · ${variantSummary(busy, status, t)}`;
  const active = status.variants.find((variant) => variant.id === status.active && variant.phase === "ready");
  if (active) return `${variantName(active.id, t)} · ${variantSummary(active, status, t)}`;
  const failed = status.variants.find((variant) => variant.phase === "failed");
  if (failed) return `${variantName(failed.id, t)} · ${variantSummary(failed, status, t)}`;
  if (status.recommended === null) {
    const reason = status.variants.find((variant) => variant.phase === "unsupported");
    return reason?.phase === "unsupported"
      ? t("这台设备不能运行本地小模型：{reason}", "This computer can't run the local model: {reason}", {
        reason: unavailableReason(reason.reason, t)
      })
      : t("这台设备不能运行本地小模型", "This computer can't run the local model");
  }
  return t("未下载模型", "No model downloaded");
}

/**
 * Appearance → Local model: the two uses of the local helper model, the
 * build it runs (picked by what this machine can run), and its prompts.
 */
export function LocalModelSettings({ preferences, onChange }: LocalModelSettingsProps): JSX.Element {
  const { t } = useI18n();
  const status = useSyncExternalStore(localModelController.subscribe, localModelController.current);
  const [confirming, setConfirming] = useState<Use | null>(null);
  const [managing, setManaging] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [unreadable, setUnreadable] = useState(false);
  const download = useDownload((message) => setError(message));

  useEffect(() => {
    localModelController.refresh().catch(() => setUnreadable(true));
  }, []);

  const unsupported = status !== null && status.recommended === null;
  const present = status?.variants.some(
    (variant) => variant.phase === "ready" || variant.phase === "downloading" || variant.phase === "preparing"
  ) ?? false;

  const setUse = (use: Use, checked: boolean): void => {
    if (checked && !present) {
      // Turning a use on needs a model: pick a build and agree to the download.
      setConfirming(use);
      return;
    }
    onChange((current) => ({ ...current, [use]: checked }));
  };

  const confirmDownload = (variant: LocalModelVariantId): void => {
    const use = confirming;
    setConfirming(null);
    if (!use) return;
    setError(null);
    download.start(variant, () => onChange((current) => ({ ...current, [use]: true })));
  };

  return (
    <SettingsCard title={t("本地小模型", "Local model")}>
      <SettingRow
        title={t("自动生成会话标题", "Name conversations automatically")}
        description={t(
          "会话第一次发出请求时，用本机运行的 Qwen3.5-0.8B 起一个标题。",
          "When a conversation sends its first request, Qwen3.5-0.8B running on this computer names it."
        )}
      >
        <Switch
          label={t("自动生成会话标题", "Name conversations automatically")}
          checked={preferences.titles}
          disabled={unsupported}
          onChange={(checked) => setUse("titles", checked)}
        />
      </SettingRow>
      <SettingRow
        title={t("解释 Shell 命令", "Explain shell commands")}
        description={t(
          "为每条 shell 命令写一行说明，显示在工具卡片的标题上。",
          "Write a one-line description of each shell command and show it as the tool card's title."
        )}
      >
        <Switch
          label={t("解释 Shell 命令", "Explain shell commands")}
          checked={preferences.shellExplanations}
          disabled={unsupported}
          onChange={(checked) => setUse("shellExplanations", checked)}
        />
      </SettingRow>
      <SettingRow
        title={t("模型与提示词", "Model and prompts")}
        description={!status && unreadable ? t("无法读取模型状态", "Couldn't read the model's status") : statusSummary(status, t)}
      >
        <button type="button" className="button button--secondary button--small" onClick={() => setManaging(true)}>
          {t("管理…", "Manage…")}
        </button>
      </SettingRow>
      {error && (
        <p className="appearance-settings-page__theme-error" role="alert">{error}</p>
      )}
      {confirming && status && (
        <ChooseVariantDialog status={status} onCancel={() => setConfirming(null)} onDownload={confirmDownload} />
      )}
      {download.question}
      {managing && (
        <LocalModelDialog
          status={status}
          preferences={preferences}
          onChange={onChange}
          onClose={() => setManaging(false)}
        />
      )}
    </SettingsCard>
  );
}

/** Before the first download: which build, out of what this machine can run. */
function ChooseVariantDialog({
  status,
  onCancel,
  onDownload
}: {
  status: LocalModelStatus;
  onCancel: () => void;
  onDownload: (variant: LocalModelVariantId) => void;
}): JSX.Element {
  const { t } = useI18n();
  const [choice, setChoice] = useState<LocalModelVariantId | null>(status.recommended);
  const machine = machineSummary(status.machine, t);
  return (
    <Dialog
      title={t("下载本地模型？", "Download the local model?")}
      description={t(
        "Qwen3.5-0.8B 官方指令版（非 Base），由官方 BF16 权重预先转换好，从 Hugging Face 下载。模型在本机运行，不会把会话内容发给任何服务；每个文件都会校验 SHA-256，下载中断后从断点继续。",
        "Qwen3.5-0.8B, the official instruct model (not Base), converted ahead of time from the official BF16 weights and downloaded from Hugging Face. It runs on this computer and sends nothing anywhere; every file is checked against its SHA-256, and an interrupted download picks up where it stopped."
      )}
      onClose={onCancel}
      width="520px"
      footer={(
        <>
          <button type="button" className="button button--secondary" onClick={onCancel}>
            {t("取消", "Cancel")}
          </button>
          <button type="button" className="button button--primary" disabled={choice === null} onClick={() => choice && onDownload(choice)}>
            {t("下载", "Download")}
          </button>
        </>
      )}
    >
      {machine && (
        <p className="local-model-dialog__machine">{t("本机：{machine}", "This Mac: {machine}", { machine })}</p>
      )}
      <div className="local-model-choice" role="radiogroup" aria-label={t("模型版本", "Model build")}>
        {status.variants.map((variant) => {
          const disabled = variant.phase === "unsupported";
          return (
            <label
              key={variant.id}
              className={`local-model-choice__option${disabled ? " local-model-choice__option--disabled" : ""}`}
            >
              <input
                type="radio"
                name="local-model-variant"
                value={variant.id}
                checked={choice === variant.id}
                disabled={disabled}
                onChange={() => setChoice(variant.id)}
              />
              <span className="local-model-choice__copy">
                <span className="local-model-choice__title">
                  {variantName(variant.id, t)}
                  {status.recommended === variant.id && (
                    <span className="local-model-choice__badge">{t("推荐", "Recommended")}</span>
                  )}
                </span>
                <small>{variantDescription(variant.id, t)}</small>
                <small className="local-model-choice__meta">
                  {variant.phase === "unsupported"
                    ? unavailableReason(variant.reason, t)
                    : t("下载约 {size}", "About {size} to download", { size: formatBytes(variant.downloadBytes) })}
                </small>
              </span>
            </label>
          );
        })}
      </div>
    </Dialog>
  );
}

/**
 * Starts downloads. On a system set to Chinese for mainland China, where
 * Hugging Face is often slow or unreachable, it first asks whether to use the
 * mirror there; everyone else downloads at once. `question` is the
 * dialog to render while it asks.
 */
function useDownload(onError: (message: string) => void): {
  start: (variant: LocalModelVariantId, onStarted?: () => void) => void;
  question: JSX.Element | null;
} {
  const [asking, setAsking] = useState<{ variant: LocalModelVariantId; onStarted?: () => void } | null>(null);
  const install = (variant: LocalModelVariantId, chinaMirror: boolean, onStarted?: () => void): void => {
    onStarted?.();
    localModelController.install(variant, chinaMirror).catch((reason: unknown) => onError(String(reason)));
  };
  const start = (variant: LocalModelVariantId, onStarted?: () => void): void => {
    if (isMainlandChinaLocale()) setAsking({ variant, onStarted });
    else install(variant, false, onStarted);
  };
  const question = asking && (
    <ChinaMirrorDialog
      onCancel={() => setAsking(null)}
      onChoose={(chinaMirror) => {
        setAsking(null);
        install(asking.variant, chinaMirror, asking.onStarted);
      }}
    />
  );
  return { start, question };
}

function ChinaMirrorDialog({
  onCancel,
  onChoose
}: {
  onCancel: () => void;
  onChoose: (chinaMirror: boolean) => void;
}): JSX.Element {
  const { t } = useI18n();
  return (
    <Dialog
      title={t("使用国内镜像下载？", "Download from the mirror in mainland China?")}
      description={t(
        "在中国大陆访问 Hugging Face 可能很慢或连不上。国内镜像 hf-mirror.com 提供同样的文件，每个文件都会校验 SHA-256。",
        "Hugging Face can be slow or unreachable from mainland China. The mirror hf-mirror.com serves the same files; every file is checked against its SHA-256."
      )}
      onClose={onCancel}
      width="480px"
      footer={(
        <>
          <button type="button" className="button button--secondary" onClick={() => onChoose(false)}>
            {t("直接下载", "Download directly")}
          </button>
          <button type="button" className="button button--primary" onClick={() => onChoose(true)}>
            {t("使用国内镜像", "Use the mirror")}
          </button>
        </>
      )}
    />
  );
}

function LocalModelDialog({
  status,
  preferences,
  onChange,
  onClose
}: {
  status: LocalModelStatus | null;
  preferences: LocalModelPreferences;
  onChange: LocalModelSettingsProps["onChange"];
  onClose: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const [defaults, setDefaults] = useState<{ title: string; shell: string } | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    // Pushed statuses leave out the runtime (device, slots); ask for it once.
    localModelController.refresh().catch(() => {});
    localModelController
      .defaultPrompts()
      .then((prompts) => {
        if (!cancelled) setDefaults(prompts);
      })
      .catch((reason: unknown) => {
        if (!cancelled) setError(String(reason));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const run = (action: () => Promise<void>): void => {
    setError(null);
    action().catch((reason: unknown) => setError(String(reason)));
  };
  const download = useDownload((message) => setError(message));

  const machine = status ? machineSummary(status.machine, t) : null;
  const installing = status?.variants.some((variant) => variant.phase === "downloading" || variant.phase === "preparing") ?? false;
  const active = status?.variants.find((variant) => variant.id === status.active && variant.phase === "ready") ?? null;

  return (
    <Dialog
      title={t("本地小模型", "Local model")}
      onClose={onClose}
      width="760px"
      footer={(
        <button type="button" className="button button--secondary" onClick={onClose}>
          {t("完成", "Done")}
        </button>
      )}
    >
      <section className="local-model-dialog__section" aria-label={t("模型", "Models")}>
        <div className="local-model-dialog__model-copy">
          <strong>Qwen3.5-0.8B</strong>
          <small>
            {t(
              "官方指令版（非 Base），由官方 BF16 权重转换而来。每个版本对应一种推理后端，可以都下载，同一时间只用其中一个。",
              "The official instruct model (not Base), converted from the official BF16 weights. Each build runs on one inference backend; download any of them and pick the one in use."
            )}
          </small>
          {machine && <small className="local-model-dialog__status">{t("本机：{machine}", "This Mac: {machine}", { machine })}</small>}
        </div>
        {status === null && <small className="local-model-dialog__info">{t("正在读取状态…", "Reading status…")}</small>}
        {status?.variants.map((variant) => (
          <VariantRow
            key={variant.id}
            variant={variant}
            status={status}
            otherInstalling={installing && variant.phase !== "downloading" && variant.phase !== "preparing"}
            run={run}
            onDownload={() => {
              setError(null);
              download.start(variant.id);
            }}
          />
        ))}
        {error && <p className="appearance-settings-page__theme-error" role="alert">{error}</p>}
      </section>

      <PromptEditor
        task="title"
        title={t("会话标题的前置提示词", "Prompt for conversation titles")}
        value={preferences.titlePrompt}
        fallback={defaults?.title ?? null}
        backend={active?.id ?? null}
        onChange={(text) => onChange((current) => ({ ...current, titlePrompt: text }))}
      />
      <PromptEditor
        task="shell"
        title={t("命令说明的前置提示词", "Prompt for command explanations")}
        value={preferences.shellPrompt}
        fallback={defaults?.shell ?? null}
        backend={active?.id ?? null}
        onChange={(text) => onChange((current) => ({ ...current, shellPrompt: text }))}
      />
      {download.question}
    </Dialog>
  );
}

function VariantRow({
  variant,
  status,
  otherInstalling,
  run,
  onDownload
}: {
  variant: LocalModelVariantStatus;
  status: LocalModelStatus;
  /** Another build is downloading: one download at a time. */
  otherInstalling: boolean;
  run: (action: () => Promise<void>) => void;
  onDownload: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const [confirmRemove, setConfirmRemove] = useState(false);
  const phase = variant.phase;
  const inUse = status.active === variant.id && phase === "ready";
  const progress = variant.phase === "downloading"
    ? percentOf(variant.received, variant.total)
    : variant.phase === "preparing" && variant.total > 0
      ? percentOf(variant.done, variant.total)
      : null;
  const name = variantName(variant.id, t);

  return (
    <div className={`local-model-variant${inUse ? " local-model-variant--active" : ""}`} aria-label={name} role="group">
      <div className="local-model-dialog__model">
        <div className="local-model-dialog__model-copy">
          <strong>
            {name}
            {inUse && <span className="local-model-choice__badge">{t("使用中", "In use")}</span>}
            {!inUse && status.recommended === variant.id && phase !== "ready" && (
              <span className="local-model-choice__badge local-model-choice__badge--quiet">{t("推荐", "Recommended")}</span>
            )}
          </strong>
          <small>{variantDescription(variant.id, t)}</small>
          <small className="local-model-dialog__status">{variantSummary(variant, status, t)}</small>
          {inUse && status.loaded && !status.warming && (
            <small>
              {t("已加载 · 最多同时 {slots} 个请求 · 每个请求 {context} 个位置", "Loaded · up to {slots} requests at once · {context} positions each", {
                slots: status.slots,
                context: status.context
              })}
            </small>
          )}
        </div>
        <div className="local-model-dialog__actions">
          {(phase === "missing" || phase === "failed") && (
            <button
              type="button"
              className="button button--primary button--small"
              disabled={otherInstalling}
              onClick={onDownload}
            >
              {phase === "failed" ? t("重试", "Retry") : t("下载", "Download")}
            </button>
          )}
          {phase === "ready" && !inUse && !confirmRemove && (
            <button type="button" className="button button--primary button--small" onClick={() => run(() => localModelController.activate(variant.id))}>
              {t("使用", "Use")}
            </button>
          )}
          {phase === "ready" && !confirmRemove && (
            <button type="button" className="button button--secondary button--small" onClick={() => setConfirmRemove(true)}>
              {t("删除", "Remove")}
            </button>
          )}
          {phase === "ready" && confirmRemove && (
            <>
              <button type="button" className="button button--secondary button--small" onClick={() => setConfirmRemove(false)}>
                {t("取消", "Cancel")}
              </button>
              <button
                type="button"
                className="button button--danger button--small"
                onClick={() => {
                  setConfirmRemove(false);
                  run(() => localModelController.remove(variant.id));
                }}
              >
                {t("确定删除", "Remove")}
              </button>
            </>
          )}
        </div>
      </div>
      {progress !== null && (
        <div className="local-model-dialog__progress-row">
          <div
            className="local-model-dialog__progress"
            role="progressbar"
            aria-label={t("{name}的安装进度", "{name} install progress", { name })}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={progress}
          >
            <span style={{ width: `${progress}%` }} />
          </div>
          <span className="local-model-dialog__progress-text">
            {variant.phase === "downloading"
              ? `${formatBytes(variant.received)} / ${formatBytes(variant.total)}`
              : `${progress}%`}
          </span>
          {variant.phase === "downloading" && (
            <IconButton label={t("取消下载", "Cancel download")} onClick={() => run(() => localModelController.cancelInstall())}>
              <X size={14} />
            </IconButton>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * One system prompt. Whatever text is in effect is cached as the model's KV
 * cache once typing settles; the line under it reports that cache for the
 * build in use (each backend keeps its own form of it).
 */
function PromptEditor({
  task,
  title,
  value,
  fallback,
  backend,
  onChange
}: {
  task: "title" | "shell";
  title: string;
  /** The stored prompt; empty means the built-in one. */
  value: string;
  fallback: string | null;
  /** The build in use, or null when none is installed. */
  backend: LocalModelVariantId | null;
  onChange: (text: string) => void;
}): JSX.Element {
  const { t } = useI18n();
  const text = value.trim() ? value : fallback ?? "";
  const [report, setReport] = useState<LocalModelPromptReport | null>(null);
  const [computing, setComputing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const request = useRef(0);

  useEffect(() => {
    if (!backend || !text.trim()) return;
    const id = ++request.current;
    setComputing(true);
    const timer = window.setTimeout(() => {
      localModelController
        .promptInfo(task, text)
        .then((next) => {
          if (request.current !== id) return;
          setReport(next);
          setError(null);
        })
        .catch((reason: unknown) => {
          if (request.current !== id) return;
          setReport(null);
          setError(String(reason));
        })
        .finally(() => {
          if (request.current === id) setComputing(false);
        });
    }, PROMPT_SETTLE_MS);
    return () => window.clearTimeout(timer);
  }, [backend, task, text]);

  let info: string;
  if (!backend) {
    info = t("安装模型后显示这段提示词的 token 数与 KV 缓存大小。", "Install the model to see this prompt's token count and KV cache size.");
  } else if (computing) {
    info = t("正在计算 KV 缓存…", "Computing the KV cache…");
  } else if (report) {
    info = t("相当于 {tokens} 个 token · KV 缓存 {size}（上限 {max} 个 token）", "{tokens} tokens · KV cache {size} (limit {max} tokens)", {
      tokens: report.tokens,
      size: formatBytes(report.cacheBytes),
      max: report.maxTokens
    });
  } else {
    info = "";
  }

  return (
    <section className="local-model-dialog__section" aria-label={title}>
      <div className="local-model-dialog__prompt-heading">
        <strong>{title}</strong>
        <button
          type="button"
          className="button button--ghost button--small"
          disabled={!value.trim()}
          onClick={() => onChange("")}
        >
          {t("恢复默认", "Reset to default")}
        </button>
      </div>
      <textarea
        className="input local-model-dialog__prompt"
        aria-label={title}
        spellCheck={false}
        value={text}
        placeholder={fallback === null ? t("正在读取默认提示词…", "Reading the default prompt…") : undefined}
        onChange={(event) => {
          const next = event.target.value;
          onChange(fallback !== null && next === fallback ? "" : next);
        }}
      />
      {error ? (
        <small className="local-model-dialog__info local-model-dialog__info--error" role="alert">{error}</small>
      ) : (
        info && <small className="local-model-dialog__info">{info}</small>
      )}
    </section>
  );
}

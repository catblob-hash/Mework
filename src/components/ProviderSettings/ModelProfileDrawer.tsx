import { useI18n } from "../../i18n";
import {
  promptCacheTakesEffect,
  reasoningContentTakesEffect,
  REASONING_CONTENTS
} from "../../lib/modelCapabilities";
import type { ProviderFamily, ModelProfile, ReasoningContent } from "../../types";
import { Field, Switch } from "../Common";
import { capabilityIcon, capabilityLabel } from "./capabilityMeta";
import { Drawer } from "./Drawer";

function optionalPositiveInteger(value: string): number | undefined {
  if (!value.trim()) return undefined;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? Math.max(1, Math.floor(parsed)) : undefined;
}

export function ModelProfileDrawer({
  providerName,
  family,
  mode,
  draft,
  idError,
  onChange,
  onClose,
  onSave
}: {
  providerName: string;
  family: ProviderFamily;
  mode: "create" | "edit";
  draft: ModelProfile;
  idError: string | null;
  onChange: (draft: ModelProfile) => void;
  onClose: () => void;
  onSave: () => void;
}) {
  const { t } = useI18n();
  /** Only the Responses family can control the reasoning-field form; other families are determined by the upstream. */
  const reasoningHint = reasoningContentTakesEffect(family)
    ? draft.reasoningContent === "plaintext"
      ? t(
          "明文：不向上游索取加密思考字段。中转站包出来的 Responses 端点多数是这一档。",
          "Plaintext: never asks the upstream for an encrypted reasoning field. Most relayed Responses endpoints work this way."
        )
      : t(
          "加密：向上游索取加密思考字段，思考因此能在同一回合的工具往返之间存活。",
          "Encrypted: asks the upstream for an encrypted reasoning field, so thinking survives tool round-trips within a turn."
        )
    : t(
        "这一档今天只对 Responses 协议有效。别的协议里思考怎么回来由上游单方面决定，客户端没有可发的开关。",
        "This setting only takes effect on the Responses protocol today. On the others the upstream alone decides how thinking comes back; there is no switch to send."
      );

  const reasoningContentLabel = (value: ReasoningContent) => {
    switch (value) {
      case "plaintext":
        return t("明文思考字段", "Plaintext reasoning field");
      case "encrypted":
        return t("加密思考字段", "Encrypted reasoning field");
    }
  };

  const visionLabel = capabilityLabel(t, "image_recognition");
  const promptCacheLabel = t("提示词缓存", "Prompt caching");
  /** Only the Messages protocol takes client-placed cache breakpoints; elsewhere the value is stored but idle. */
  const promptCacheHint = promptCacheTakesEffect(family)
    ? t(
        "按 Claude Code 的规则在请求里打缓存断点：系统提示词的稳定前缀与本步尾巴各一处，最后一条可打标的消息一处。Claude 只缓存客户端标记过的前缀，关掉就完全没有缓存。",
        "Places cache breakpoints the way Claude Code does: one on the stable system-prompt prefix, one on its per-step tail, and one on the last markable message. Claude only caches what the client marks, so turning this off means no caching at all."
      )
    : t(
        "这一档今天只对 Anthropic Messages 协议有效；别的协议由上游自行决定缓存，客户端没有可发的标记。",
        "This setting only takes effect on the Anthropic Messages protocol today; on the others the upstream decides caching on its own and there is no marker to send."
      );

  return (
    <Drawer
      title={mode === "create" ? t("添加模型", "Add model") : t("模型属性", "Model properties")}
      subtitle={t("{provider} · 修改只会在保存后应用", "{provider} · Changes apply only after saving", { provider: providerName })}
      labelledBy="model-editor-title"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="button button--ghost" onClick={onClose}>{t("取消", "Cancel")}</button>
          <button type="button" className="button button--primary" disabled={Boolean(idError)} onClick={onSave}>{t("保存", "Save")}</button>
        </>
      }
    >
      <Field label={t("模型 ID", "Model ID")} hint={idError ?? t("请求 API 时发送的 model 值", "The model value sent in API requests")}>
        <input
          className={`input input--code${idError ? " input--error" : ""}`}
          aria-label={t("模型 ID", "Model ID")}
          aria-invalid={Boolean(idError)}
          value={draft.id}
          onChange={(event) => onChange({ ...draft, id: event.target.value })}
          placeholder={t("例如 gpt-5", "For example, gpt-5")}
        />
      </Field>
      <div className="drawer-field-grid">
        <Field label={t("显示名", "Display name")} hint={t("留空则显示模型 ID", "Falls back to the model ID when blank")}>
          <input className="input" aria-label={t("显示名", "Display name")} value={draft.name} onChange={(event) => onChange({ ...draft, name: event.target.value })} />
        </Field>
        <Field label={t("分组", "Group")} hint={t("模型列表里的折叠分组；留空则由 ID 推断", "The collapsible group in the model list; inferred from the ID when blank")}>
          <input className="input" aria-label={t("分组", "Group")} value={draft.group} onChange={(event) => onChange({ ...draft, group: event.target.value })} />
        </Field>
        <Field label={t("上下文窗口", "Context window")}>
          <input className="input" aria-label={t("上下文窗口", "Context window")} type="number" min={1} step={1} value={draft.contextWindow ?? ""} onChange={(event) => onChange({ ...draft, contextWindow: optionalPositiveInteger(event.target.value) })} placeholder={t("留空表示未知", "Leave blank if unknown")} />
        </Field>
        <Field label={t("单次最大输出", "Maximum output")}>
          <input className="input" aria-label={t("最大输出 Token", "Maximum output tokens")} type="number" min={1} step={1} value={draft.maxOutputTokens ?? ""} onChange={(event) => onChange({ ...draft, maxOutputTokens: optionalPositiveInteger(event.target.value) })} placeholder={t("留空表示未知", "Leave blank if unknown")} />
        </Field>
      </div>

      <div className="drawer-switch-row">
        <span>
          <strong>{capabilityIcon("image_recognition", 12)} {visionLabel}</strong>
          <small>{t("允许把图片作为输入发给这个模型。", "Lets images be sent to this model as input.")}</small>
        </span>
        <Switch
          checked={draft.capabilities.includes("image_recognition")}
          onChange={(on) => onChange({ ...draft, capabilities: on ? ["image_recognition"] : [] })}
          label={t("{name} {capability}", "{name} {capability}", {
            name: draft.id || t("新模型", "New model"),
            capability: visionLabel
          })}
        />
      </div>

      <div className="drawer-switch-row">
        <span>
          <strong>{promptCacheLabel}</strong>
          <small>{promptCacheHint}</small>
        </span>
        <Switch
          checked={draft.promptCache}
          onChange={(on) => onChange({ ...draft, promptCache: on })}
          label={t("{name} {capability}", "{name} {capability}", {
            name: draft.id || t("新模型", "New model"),
            capability: promptCacheLabel
          })}
        />
      </div>

      <Field label={t("思考字段形态", "Reasoning field form")} hint={reasoningHint}>
        <select
          className="input"
          aria-label={t("思考字段形态", "Reasoning field form")}
          value={draft.reasoningContent}
          onChange={(event) => onChange({ ...draft, reasoningContent: event.target.value as ReasoningContent })}
        >
          {REASONING_CONTENTS.map((value) => (
            <option key={value} value={value}>{reasoningContentLabel(value)}</option>
          ))}
        </select>
      </Field>
    </Drawer>
  );
}

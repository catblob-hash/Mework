import { AlertTriangle, Bot, BrainCircuit, Check, LoaderCircle, Lock, Play, Shield, UserRound, Wrench } from "lucide-react";
import { useMemo, useState } from "react";
import { getI18nSnapshot, translate, useI18n } from "../i18n";
import type { ContextItem, ImageAttachment, InsertableContextKind, JsonObject, JsonValue, ToolContext, ToolDescriptor, ToolResult } from "../types";
import { Dialog, Field, Switch } from "./Common";
import { DiffOutput } from "./DiffOutput";
import { ImageStrip } from "./ImageStrip";

type DraftValue = string | boolean;

interface ContextEditorProps {
  mode: "insert" | "edit";
  kind: InsertableContextKind;
  item?: ContextItem;
  tools: ToolDescriptor[];
  enabledTools: string[];
  onClose: () => void;
  onSaveText: (content: string, images?: ImageAttachment[]) => void;
  onSaveTool: (toolName: string, input: JsonObject) => Promise<ToolResult>;
  onSaveToolImages?: (images: ImageAttachment[]) => void;
}

const textMeta: Record<Exclude<InsertableContextKind, "tool">, {
  title: (t: ReturnType<typeof useI18n>["t"]) => string;
  description: (t: ReturnType<typeof useI18n>["t"]) => string;
  placeholder: (t: ReturnType<typeof useI18n>["t"]) => string;
  icon: typeof Shield;
}> = {
  system: {
    title: (t) => t("系统提示词上下文", "System prompt context"),
    description: (t) => t("这是一条位于时间线中的系统指令，不会替换“本对话设置”里的主系统提示词。", "This is a system instruction in the timeline. It does not replace the main system prompt in Conversation settings."),
    placeholder: (t) => t("输入要在这个位置生效的系统指令…", "Enter a system instruction to apply at this point…"),
    icon: Shield
  },
  user: {
    title: (t) => t("用户输入", "User input"),
    description: (t) => t("插入或修改一条用户消息。", "Insert or edit a user message."),
    placeholder: (t) => t("输入用户消息…", "Enter a user message…"),
    icon: UserRound
  },
  reasoning: {
    title: (t) => t("明文思考字段", "Plain-text reasoning field"),
    description: (t) => t("手动添加的思考是可编辑的明文。加密思考只能由模型提供方产生。", "Manually added reasoning is editable plain text. Encrypted reasoning can only be produced by the model provider."),
    placeholder: (t) => t("输入推理过程或计划…", "Enter reasoning or a plan…"),
    icon: BrainCircuit
  },
  assistant: {
    title: (t) => t("模型回复", "Model reply"),
    description: (t) => t("插入或修改一条模型回复。", "Insert or edit a model reply."),
    placeholder: (t) => t("输入模型回复…", "Enter a model reply…"),
    icon: Bot
  }
};

function toDraft(tool: ToolDescriptor, input?: JsonObject): Record<string, DraftValue> {
  return Object.fromEntries(
    tool.parameters.map((parameter) => {
      const value = input?.[parameter.name] ?? parameter.defaultValue ?? (parameter.type === "boolean" ? false : "");
      // Keep booleans typed: String(false) becomes the non-empty "false", and
      // Boolean("false") becomes true, incorrectly enabling untouched controls.
      if (parameter.type === "boolean") return [parameter.name, Boolean(value)];
      return [parameter.name, parameter.type === "json" && typeof value !== "string" ? JSON.stringify(value, null, 2) : String(value ?? "")];
    })
  );
}

function parseDraft(
  tool: ToolDescriptor,
  draft: Record<string, DraftValue>,
  messages = {
    required: translate(getI18nSnapshot().resolvedLanguage, "此项为必填", "This field is required"),
    number: translate(getI18nSnapshot().resolvedLanguage, "请输入有效数字", "Enter a valid number"),
    json: translate(getI18nSnapshot().resolvedLanguage, "JSON 格式不正确", "Invalid JSON")
  }
): { input?: JsonObject; errors: Record<string, string> } {
  const input: JsonObject = {};
  const errors: Record<string, string> = {};
  tool.parameters.forEach((parameter) => {
    const raw = draft[parameter.name];
    if (parameter.required && (raw === "" || raw === undefined)) {
      errors[parameter.name] = messages.required;
      return;
    }
    if (raw === "" || raw === undefined) return;
    if (parameter.type === "number") {
      const parsed = Number(raw);
      if (Number.isNaN(parsed)) errors[parameter.name] = messages.number;
      else input[parameter.name] = parsed;
      return;
    }
    if (parameter.type === "boolean") {
      // Omit optional booleans matching their declared defaults: absent values
      // carry the default schema semantics and avoid recording untouched arguments.
      const fallback = typeof parameter.defaultValue === "boolean" ? parameter.defaultValue : false;
      if (!parameter.required && Boolean(raw) === fallback) return;
      input[parameter.name] = Boolean(raw);
      return;
    }
    if (parameter.type === "json") {
      try {
        input[parameter.name] = JSON.parse(String(raw)) as JsonValue;
      } catch {
        errors[parameter.name] = messages.json;
      }
      return;
    }
    input[parameter.name] = String(raw);
  });
  return Object.keys(errors).length ? { errors } : { input, errors };
}

function ToolParameterField({
  parameter,
  value,
  error,
  onChange
}: {
  parameter: ToolDescriptor["parameters"][number];
  value: DraftValue;
  error?: string;
  onChange: (value: DraftValue) => void;
}) {
  const label = `${parameter.label}${parameter.required ? " *" : ""}`;
  if (parameter.type === "boolean") {
    return (
      <div className="switch-field">
        <div>
          <span>{label}</span>
          {parameter.help && <small>{parameter.help}</small>}
        </div>
        <Switch checked={Boolean(value)} onChange={onChange} label={parameter.label} />
      </div>
    );
  }
  const multiline = parameter.type === "multiline" || parameter.type === "json";
  return (
    <Field label={label} hint={error || parameter.help}>
      {multiline ? (
        <textarea
          className={`input ${error ? "input--error" : ""} ${parameter.type === "json" ? "input--code" : ""}`}
          rows={parameter.type === "json" ? 5 : 4}
          value={String(value)}
          placeholder={parameter.placeholder}
          onChange={(event) => onChange(event.target.value)}
          aria-invalid={Boolean(error)}
        />
      ) : (
        <input
          className={`input ${error ? "input--error" : ""}`}
          type={parameter.type === "number" ? "number" : "text"}
          value={String(value)}
          placeholder={parameter.placeholder}
          onChange={(event) => onChange(event.target.value)}
          aria-invalid={Boolean(error)}
        />
      )}
    </Field>
  );
}

export function ContextEditor({
  mode,
  kind,
  item,
  tools,
  enabledTools,
  onClose,
  onSaveText,
  onSaveTool,
  onSaveToolImages
}: ContextEditorProps) {
  const { t } = useI18n();
  const textItem = item && item.kind !== "tool" ? item : undefined;
  const [content, setContent] = useState(textItem && "content" in textItem ? textItem.content ?? "" : "");
  const [textImages, setTextImages] = useState<ImageAttachment[]>(
    textItem?.kind === "user" ? textItem.images ?? [] : []
  );
  const existingTool = item?.kind === "tool" ? item : undefined;
  const [toolImages, setToolImages] = useState<ImageAttachment[]>(existingTool?.result.images ?? []);
  // Orchestration tools (subagents, workflow, todo state, ask_user) run
  // only inside the model loop.
  const availableTools = useMemo(
    () => tools.filter((tool) =>
      tool.category !== "orchestration"
      && enabledTools.includes(tool.name)),
    [tools, enabledTools]
  );
  const [toolName, setToolName] = useState(existingTool?.toolName ?? availableTools[0]?.name ?? "");
  const selectedTool = tools.find((tool) => tool.name === toolName);
  const editingQuestion = mode === "edit" && existingTool?.toolName === "ask_user";
  const [draft, setDraft] = useState<Record<string, DraftValue>>(() =>
    selectedTool ? toDraft(selectedTool, existingTool?.input) : {}
  );
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [running, setRunning] = useState(false);
  const [runError, setRunError] = useState<string | null>(null);

  if (kind !== "tool") {
    const meta = textMeta[kind];
    const title = meta.title(t);
    const Icon = meta.icon;
    const keepsImages = kind === "user" && Boolean(textImages.length);
    const save = () => {
      const trimmed = content.trim();
      if (!trimmed && !keepsImages) return;
      onSaveText(trimmed, kind === "user" ? textImages : undefined);
    };
    return (
      <Dialog
        title={mode === "insert" ? t("添加{title}", "Add {title}", { title }) : t("编辑{title}", "Edit {title}", { title })}
        description={meta.description(t)}
        onClose={onClose}
        footer={
          <>
            <button type="button" className="button button--ghost" onClick={onClose}>{t("取消", "Cancel")}</button>
            <button type="button" className="button button--primary" disabled={!content.trim() && !keepsImages} onClick={save}>
              <Check size={15} /> {t("保存", "Save")}
            </button>
          </>
        }
      >
        <div className={`editor-kind editor-kind--${kind}`}>
          <Icon size={16} />
          <span>{title}</span>
        </div>
        {textItem?.kind === "user" && (
          <ImageStrip
            images={textImages}
            compact
            className="context-editor__images"
            onRemove={(imageId) => {
              setTextImages((current) => current.filter((image) => image.id !== imageId));
            }}
          />
        )}
        <textarea
          className="context-text-editor"
          rows={10}
          autoFocus
          value={content}
          placeholder={meta.placeholder(t)}
          onChange={(event) => setContent(event.target.value)}
          onKeyDown={(event) => {
            if ((event.ctrlKey || event.metaKey) && event.key === "Enter") save();
          }}
        />
        <div className="editor-counter">
          <span>{t("⌘/Ctrl + Enter 保存", "⌘/Ctrl + Enter to save")}</span>
          <span>{t("{count} 字符", "{count} characters", { count: content.length })}</span>
        </div>
      </Dialog>
    );
  }

  const submitTool = async () => {
    if (!selectedTool) return;
    const parsed = parseDraft(selectedTool, draft, {
      required: t("此项为必填", "This field is required"),
      number: t("请输入有效数字", "Enter a valid number"),
      json: t("JSON 格式不正确", "Invalid JSON")
    });
    setErrors(parsed.errors);
    if (!parsed.input) return;
    setRunning(true);
    setRunError(null);
    try {
      await onSaveTool(selectedTool.name, parsed.input);
    } catch (error) {
      setRunError(error instanceof Error ? error.message : String(error));
      setRunning(false);
    }
  };
  const toolImagesChanged = existingTool
    ? toolImages.length !== (existingTool.result.images?.length ?? 0)
      || toolImages.some((image, index) => image.id !== existingTool.result.images?.[index]?.id)
    : false;

  return (
    <Dialog
      title={mode === "insert" ? t("添加工具调用", "Add tool call") : t("编辑工具参数", "Edit tool arguments")}
      description={
        mode === "insert"
          ? t("选择工具并填写它需要的参数；调用标识与返回结果由系统管理。", "Choose a tool and fill in its arguments. The system manages the call ID and result.")
          : editingQuestion
            ? t("修改问题后保留现有回答与等待状态；不会重新执行工具。", "Update the questions without rerunning the tool or discarding its current answer state.")
          : t("工具名称与旧返回结果不可编辑。保存后会重新执行，并用新结果完整覆盖旧结果。", "The tool name and previous result cannot be edited. Saving runs the tool again and fully replaces the old result.")
      }
      onClose={() => !running && onClose()}
      width="700px"
      footer={
        <>
          <button type="button" className="button button--ghost" onClick={onClose} disabled={running}>{t("取消", "Cancel")}</button>
          {toolImagesChanged && onSaveToolImages ? (
            <button
              type="button"
              className="button button--ghost"
              disabled={running}
              onClick={() => {
                onSaveToolImages(toolImages);
                onClose();
              }}
            >
              <Check size={15} /> {t("保存图片修改", "Save image changes")}
            </button>
          ) : null}
          <button
            type="button"
            className="button button--primary"
            disabled={running || !selectedTool}
            onClick={submitTool}
          >
            {running ? <LoaderCircle size={15} className="spin" /> : editingQuestion ? <Check size={15} /> : <Play size={15} />}
            {running
              ? t("正在保存…", "Saving…")
              : mode === "insert"
                ? t("执行并添加", "Run and add")
                : editingQuestion ? t("保存", "Save") : t("保存并重新执行", "Save and rerun")}
          </button>
        </>
      }
    >
      <div className="tool-editor-layout">
        <div className="tool-picker-panel">
          <span className="tool-editor-label">{t("工具", "Tools")}</span>
          <div className="tool-picker-list">
            {(existingTool
              ? tools.filter((tool) => tool.name === existingTool.toolName)
              : availableTools).map((tool) => (
              <button
                type="button"
                className={tool.name === toolName ? "tool-picker-item tool-picker-item--active" : "tool-picker-item"}
                key={tool.name}
                onClick={() => {
                  if (existingTool) return;
                  setToolName(tool.name);
                  setDraft(toDraft(tool));
                  setErrors({});
                }}
              >
                <span className="tool-picker-item__icon"><Wrench size={15} /></span>
                <span><strong>{tool.label}</strong></span>
                {tool.name === toolName && <Check size={15} />}
              </button>
            ))}
          </div>
        </div>
        <div className="tool-arguments-panel">
          {selectedTool ? (
            <>
              <div className="tool-arguments-heading">
                <div><strong>{selectedTool.label}</strong><code>{selectedTool.name}</code></div>
                {existingTool && <span><Lock size={12} /> {t("工具已锁定", "Tool locked")}</span>}
              </div>
              <div className="tool-fields">
                {selectedTool.parameters.map((parameter) => (
                  <ToolParameterField
                    key={parameter.name}
                    parameter={parameter}
                    value={draft[parameter.name] ?? ""}
                    error={errors[parameter.name]}
                    onChange={(value) => setDraft((current) => ({ ...current, [parameter.name]: value }))}
                  />
                ))}
              </div>
              {selectedTool.dangerous && (
                <div className="risk-confirmation" role="note" style={{ cursor: "default" }}>
                  <AlertTriangle size={16} />
                  <span><strong>{t("需安全审查", "Safety-reviewed tool")}</strong><small>{t("后端会根据本次参数、路径与安全层级动态判定风险和批准；此标记本身不代表固定的高风险。", "The backend classifies the concrete arguments, paths, and security level at execution time; this marker is not a fixed high-risk verdict.")}</small></span>
                </div>
              )}
              {existingTool && (
                <div className="previous-result">
                  <span>
                    {editingQuestion
                      ? t("当前等待状态与回答关联会保留", "The current waiting state and answer association will be preserved")
                      : t("当前返回结果 · 保存成功后会被覆盖", "Current result · replaced after a successful save")}
                  </span>
                  <ImageStrip
                    images={toolImages}
                    compact
                    className="context-editor__images"
                    onRemove={(imageId) => {
                      setToolImages((current) => current.filter((image) => image.id !== imageId));
                    }}
                  />
                  {existingTool.result.success && existingTool.result.diff ? (
                    <DiffOutput
                      value={existingTool.result.diff}
                      path={typeof existingTool.input.path === "string" ? existingTool.input.path : undefined}
                      summary={existingTool.result.output}
                    />
                  ) : (
                    <pre>{existingTool.result.output}</pre>
                  )}
                </div>
              )}
              {runError && <div className="inline-error" role="alert">{runError}</div>}
            </>
          ) : (
            <div className="tool-editor-empty">{t("当前对话没有可用工具。请先在“本对话设置”中启用工具。", "No tools are available in this conversation. Enable tools in Conversation settings first.")}</div>
          )}
        </div>
      </div>
    </Dialog>
  );
}

export type EditableToolContext = ToolContext;

import { Check, LoaderCircle, Play, X } from "lucide-react";
import { useMemo, useState } from "react";
import { useI18n } from "../i18n";
import { canRerunTool } from "../lib/toolRerun";
import type { ImageAttachment, JsonObject, JsonValue, ToolContext, ToolDescriptor } from "../types";
import { IconButton, PlainField } from "./Common";
import { ImageStrip } from "./ImageStrip";

type Draft = Record<string, string>;

/** One editable argument row, flattened from either a descriptor or a recorded call. */
interface EditableField {
  name: string;
  /** Accessible name of the control; required arguments keep the trailing " *". */
  label: string;
  required: boolean;
  /** How the typed text becomes JSON again. */
  decode: "string" | "number" | "boolean" | "json";
  /** Shown in the empty control; the descriptor's hint at what belongs there. */
  placeholder?: string;
}

function fieldFromParameter(parameter: ToolDescriptor["parameters"][number]): EditableField {
  const defaultText = parameter.defaultValue === undefined || parameter.defaultValue === null
    ? undefined
    : typeof parameter.defaultValue === "string" ? parameter.defaultValue : JSON.stringify(parameter.defaultValue);
  return {
    name: parameter.name,
    label: `${parameter.label}${parameter.required ? " *" : ""}`,
    required: parameter.required,
    decode: parameter.type === "number"
      ? "number"
      : parameter.type === "boolean"
        ? "boolean"
        : parameter.type === "json" ? "json" : "string",
    // A declared default is only ever a hint in the empty control: the call
    // carries what was written, and an argument left out stays left out.
    placeholder: parameter.placeholder ?? defaultText
  };
}

function descriptorFields(tool: ToolDescriptor): EditableField[] {
  return tool.parameters.map(fieldFromParameter);
}

/**
 * The descriptor's arguments, plus anything the recorded call carries that the
 * descriptor no longer declares. A card outlives the catalog entry that made
 * it, and an argument that cannot be seen cannot be corrected.
 */
function recordedFields(input: JsonObject, descriptor?: ToolDescriptor): EditableField[] {
  const fields = descriptor ? descriptorFields(descriptor) : [];
  const covered = new Set(fields.map((field) => field.name));
  for (const name of Object.keys(input)) {
    if (covered.has(name)) continue;
    const recorded = input[name];
    fields.push({
      name,
      label: name,
      required: false,
      decode: typeof recorded === "boolean"
        ? "boolean"
        : typeof recorded === "string" ? "string" : "json"
    });
  }
  return fields;
}

/**
 * The controls' text, from the arguments the call carries and nothing else.
 * Filling a declared default in here would write it into the call on save — an
 * argument the model never gave, replayed to it as if it had.
 */
function draftFromInput(fields: EditableField[], input?: JsonObject): Draft {
  return Object.fromEntries(
    fields.map((field) => {
      const value = input?.[field.name];
      if (value === undefined || value === null) return [field.name, ""];
      return [
        field.name,
        field.decode === "string" && typeof value === "string" ? value : JSON.stringify(value, null, 2)
      ];
    })
  );
}

function parseDraft(
  fields: EditableField[],
  draft: Draft,
  messages: { required: string; number: string; boolean: string; json: string },
  recorded?: { input: JsonObject; draft: Draft }
): { input?: JsonObject; errors: Record<string, string> } {
  const input: JsonObject = {};
  const errors: Record<string, string> = {};
  fields.forEach((field) => {
    // A control left as it was keeps the recorded value exactly — a `null`, an
    // empty string, a number in a text field — rather than whatever the typed
    // text would decode to. Only what was edited is read back from the text.
    if (recorded && field.name in recorded.input && draft[field.name] === recorded.draft[field.name]) {
      input[field.name] = recorded.input[field.name];
      return;
    }
    const raw = (draft[field.name] ?? "").trim();
    if (!raw) {
      if (field.required) errors[field.name] = messages.required;
      return;
    }
    if (field.decode === "number") {
      const parsed = Number(raw);
      if (Number.isNaN(parsed)) errors[field.name] = messages.number;
      else input[field.name] = parsed;
      return;
    }
    if (field.decode === "boolean") {
      if (raw !== "true" && raw !== "false") {
        errors[field.name] = messages.boolean;
        return;
      }
      // Written as typed, even when it equals the default: an explicit `false`
      // the call carried is the call's, and an empty control already omits it.
      input[field.name] = raw === "true";
      return;
    }
    if (field.decode === "json") {
      try {
        input[field.name] = JSON.parse(raw) as JsonValue;
      } catch {
        errors[field.name] = messages.json;
      }
      return;
    }
    input[field.name] = draft[field.name];
  });
  return Object.keys(errors).length ? { errors } : { input, errors };
}

function ArgumentRows({
  fields,
  draft,
  errors,
  disabled,
  onChange
}: {
  fields: EditableField[];
  draft: Draft;
  errors: Record<string, string>;
  disabled: boolean;
  onChange: (name: string, value: string) => void;
}) {
  return (
    <>
      {fields.map((field) => (
        <div className="tool-kv__row" key={field.name}>
          <dt className="tool-kv__key" title={field.name}>{field.name}</dt>
          <dd className={`tool-kv__value${errors[field.name] ? " tool-kv__value--invalid" : ""}`}>
            <PlainField
              className="context-text-editor"
              value={draft[field.name] ?? ""}
              label={field.label}
              placeholder={field.placeholder}
              disabled={disabled}
              invalid={Boolean(errors[field.name])}
              onChange={(next) => onChange(field.name, next)}
            />
          </dd>
        </div>
      ))}
    </>
  );
}

function firstError(errors: Record<string, string>): string | null {
  const [first] = Object.values(errors);
  return first ?? null;
}

/** The card an insertion is drafting: the chosen tool, no arguments, and no result until it runs. */
export function draftToolContext(toolName: string): ToolContext {
  return {
    id: "draft",
    kind: "tool",
    toolName,
    input: {},
    result: { success: true, output: "", executedAt: "", durationMs: 0 },
    createdAt: ""
  };
}

export interface InlineToolEditorProps {
  item: ToolContext;
  descriptor?: ToolDescriptor;
  /**
   * The call is being placed rather than corrected. It decides only what the
   * run button offers to do; the form is the same either way, result included,
   * because a placed call may be written out by hand instead of executed.
   */
  inserting?: boolean;
  onCancel: () => void;
  /**
   * Executes the call and writes its arguments and result together. Absent on a
   * surface with nothing to execute against — a template, which belongs to no
   * conversation and no workspace — where the button is therefore not drawn.
   */
  onRun?: (toolName: string, input: JsonObject) => Promise<unknown>;
  /** Writes the arguments and result without executing anything. */
  onSave?: (input: JsonObject, output: string, images: ImageAttachment[]) => Promise<unknown>;
}

/**
 * A call as its arguments and then its result, named on the left and boxed on
 * the right — the same shape a card takes when it is only being read.
 *
 * Values are typed as text throughout, booleans included: a call is a JSON
 * document, and a control that renders `false` as an unlit switch hides the
 * difference between an argument set to false and one never given at all.
 *
 * Both arguments and result are editable, whether the card is being corrected
 * or placed for the first time. Saving them does not execute anything, so the
 * host has to issue the card's attestation over what was typed — which is also
 * what lets a call with side effects be recorded without causing them. Running
 * is offered on a placed call, and on a recorded one only for the tools
 * `canRerunTool` accepts: the ones that can be executed from the timeline
 * without touching external state. A caller that hands over no `onRun` at all
 * has nothing to run against, and gets Cancel and Save alone.
 */
export function InlineToolEditor({ item, descriptor, inserting = false, onCancel, onRun, onSave }: InlineToolEditorProps) {
  const { t } = useI18n();
  const rerunnable = !inserting && canRerunTool(item.toolName) && Boolean(descriptor);
  // The call as the model made it. A hook may have run it with other
  // arguments, but what the model is shown of its own call is what it wrote;
  // editing the executed arguments instead would, on save, hand the model a
  // rewrite as its own words even when nothing was changed.
  const callInput = item.requestedInput ?? item.input;
  const fields = useMemo(() => recordedFields(callInput, descriptor), [callInput, descriptor]);
  const [initialDraft] = useState<Draft>(() => draftFromInput(fields, callInput));
  const [draft, setDraft] = useState<Draft>(initialDraft);
  const [output, setOutput] = useState(item.result.output);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [images, setImages] = useState<ImageAttachment[]>(item.result.images ?? []);
  const [busy, setBusy] = useState<"save" | "rerun" | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  const parse = () => {
    const parsed = parseDraft(fields, draft, {
      required: t("此项为必填", "This field is required"),
      number: t("请输入有效数字", "Enter a valid number"),
      boolean: t("请输入 true 或 false", "Enter true or false"),
      json: t("JSON 格式不正确", "Invalid JSON")
    }, { input: callInput, draft: initialDraft });
    setErrors(parsed.errors);
    setFailure(firstError(parsed.errors));
    return parsed.input;
  };

  const run = async (mode: "save" | "rerun", act: (input: JsonObject) => Promise<unknown>) => {
    if (busy) return;
    const input = parse();
    if (!input) return;
    setBusy(mode);
    setFailure(null);
    try {
      await act(input);
    } catch (error) {
      setFailure(error instanceof Error ? error.message : String(error));
      setBusy(null);
    }
  };

  return (
    <div
      className="inline-tool-editor"
      data-tool-name={item.toolName}
      onKeyDown={(event) => {
        if (event.key === "Escape" && !busy) onCancel();
      }}
    >
      <dl className="tool-kv">
        <ArgumentRows
          fields={fields}
          draft={draft}
          errors={errors}
          disabled={Boolean(busy)}
          onChange={(name, value) => setDraft((current) => ({ ...current, [name]: value }))}
        />
        <div className="tool-kv__row tool-kv__row--result">
          <dd className="tool-kv__value">
            <ImageStrip
              images={images}
              compact
              className="context-editor__images"
              onRemove={(imageId) => setImages((current) => current.filter((image) => image.id !== imageId))}
            />
            <PlainField
              /* Both classes are load bearing: the first gives the box the
                 shared editor metrics, the second colours a failed result. */
              className={`context-text-editor${item.result.success ? "" : " plain-field--failed"}`}
              value={output}
              label={t("返回值", "Result")}
              disabled={Boolean(busy)}
              onChange={setOutput}
            />
          </dd>
        </div>
      </dl>

      <div className="inline-tool-editor__footer">
        {failure && <p className="inline-tool-editor__failure" role="alert">{failure}</p>}
        <IconButton label={t("取消", "Cancel")} disabled={Boolean(busy)} onClick={onCancel}>
          <X size={14} />
        </IconButton>
        {onSave && (
          <IconButton
            label={t("保存", "Save")}
            disabled={Boolean(busy)}
            onClick={() => void run("save", (input) => onSave(input, output, images))}
          >
            {busy === "save" ? <LoaderCircle size={14} className="spin" /> : <Check size={14} />}
          </IconButton>
        )}
        {onRun && (inserting || rerunnable) && (
          <IconButton
            label={inserting ? t("执行并添加", "Run and add") : t("重新执行", "Rerun")}
            disabled={Boolean(busy)}
            onClick={() => void run("rerun", (input) => onRun(item.toolName, input))}
          >
            {busy === "rerun" ? <LoaderCircle size={14} className="spin" /> : <Play size={14} />}
          </IconButton>
        )}
      </div>
    </div>
  );
}

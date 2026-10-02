import { DatabaseZap, Lock } from "lucide-react";
import type { ReactNode } from "react";
import { useI18n, type TranslationFunction } from "../i18n";
import type { LockTone } from "../lib/toolLock";
import { Switch } from "./Common";

/**
 * What a toned row says about itself (`lockTone` in `toolLock.ts`).
 *
 * Orange is a warning and the row still moves: the last request's cache is
 * warm, and moving the row throws it away. Gray is a refusal: the model cannot
 * take tools mid-conversation, so the surface stays as its first request had
 * it until another model is selected.
 */
export interface LockHints {
  cache: string;
  hard: string;
}

export function lockHints(t: TranslationFunction): LockHints {
  return {
    cache: t(
      "缓存还热：改动这一项会让它失效。",
      "The cache is still warm: changing this throws it away."
    ),
    hard: t(
      "这个模型不支持中途增减工具，首次请求后就固定了。换一个模型才能改。",
      "This model cannot take tools on or off mid-conversation, so this was fixed at the first request. Select another model to change it."
    )
  };
}

/** The class a row carries for its tone, with the leading space, or nothing. */
export function lockToneClass(base: string, tone: LockTone | null | undefined): string {
  return tone ? ` ${base}--${tone}` : "";
}

/**
 * What the conversation's lock says about a web backend selector
 * (`backendTone` in `toolLock.ts`): its tone, and the note said under it. Gray
 * cannot move; orange moves through the caller's cache-break warning.
 */
export interface BackendLock {
  tone: LockTone;
  note: string;
}

/**
 * A selector field's hint under the lock: a gray note replaces the standing
 * advice, since it is the one thing left to say about a row that cannot move;
 * an orange one is said above it, since the row still moves and the advice
 * still applies.
 */
export function lockedFieldHint(lock: BackendLock | null | undefined, standing: string): ReactNode {
  if (!lock) return standing;
  if (lock.tone === "hard") return lock.note;
  return <><span className="lock-note lock-note--cache">{lock.note}</span>{standing}</>;
}

/**
 * The composer's model menu marks each model whose prompt cache for this
 * conversation is still warm (`modelCacheWarmUntil` in `toolLock.ts`), in the
 * cache tone, with the moment it runs out on hover.
 */
export function ModelCacheMark({ until }: { until: number }) {
  const { t, resolvedLanguage } = useI18n();
  const time = new Intl.DateTimeFormat(resolvedLanguage, { hour: "2-digit", minute: "2-digit" })
    .format(new Date(until));
  const label = t("提示缓存有效，{time} 过期", "Prompt cache warm until {time}", { time });
  return (
    <span className="model-cache-mark" role="img" aria-label={label} title={label}>
      <DatabaseZap size={12} aria-hidden="true" />
    </span>
  );
}

/** The trailing lock a toned row carries, in the tone's color. */
export function LockMark({ tone, className }: { tone: LockTone | null | undefined; className?: string }) {
  if (!tone) return null;
  return <Lock className={`lock-mark lock-mark--${tone}${className ? ` ${className}` : ""}`} size={13} aria-hidden="true" />;
}

/**
 * A switch row the conversation's lock may tone: a title, a line saying what the
 * switch does, and the note for its tone below that. A gray row cannot move; an
 * orange one moves through the caller's cache-break warning, which the caller
 * owns — every page shares one.
 */
export function LockableSwitchRow({
  title,
  description,
  checked,
  onChange,
  label,
  tone = null,
  hints,
  disabled = false,
  disabledNote
}: {
  title: string;
  description: ReactNode;
  checked: boolean;
  onChange: (checked: boolean) => void;
  /** The switch's accessible name, which says its state. */
  label: string;
  tone?: LockTone | null;
  hints?: LockHints;
  /** Off for a reason that is not the lock's; `disabledNote` says which. */
  disabled?: boolean;
  disabledNote?: string;
}) {
  const note = disabled ? disabledNote : tone && hints?.[tone];
  return (
    <div className={`tool-toggle-row${lockToneClass("tool-toggle-row", disabled ? "hard" : tone)}`}>
      <span>
        <strong>{title}</strong>
        <small>{description}</small>
        {note && <small className={`lock-note${lockToneClass("lock-note", disabled ? "hard" : tone)}`}>{note}</small>}
      </span>
      <LockMark tone={disabled ? null : tone} />
      <Switch
        checked={checked}
        disabled={disabled || tone === "hard"}
        tone={tone === "cache" ? "cache" : undefined}
        onChange={onChange}
        label={label}
      />
    </div>
  );
}

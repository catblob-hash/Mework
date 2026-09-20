import { useState } from "react";
import { useI18n } from "../i18n";
import {
  SEARCH_COMPRESSION_CUTOFF_CEILING,
  SEARCH_MAX_RESULTS_CEILING
} from "../lib/searchProviders";
import { Field } from "./Common";

/**
 * A whole-number row whose value is edited in place.
 *
 * The field cannot render the committed number back while it is being typed:
 * clearing it to type a different one would put the old value straight back
 * under the cursor. The draft holds what was typed, and a keystroke that reads
 * as a number commits it while one that does not — an empty field mid-edit —
 * simply leaves the last committed value alone.
 */
function NumberField({
  label,
  hint,
  value,
  ceiling,
  onChange
}: {
  label: string;
  hint: string;
  value: number;
  ceiling: number;
  onChange: (next: number) => void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  return (
    <Field label={label} hint={hint}>
      <input
        className="input"
        type="number"
        min={0}
        max={ceiling}
        step={1}
        aria-label={label}
        value={draft ?? String(value)}
        onChange={(event) => {
          setDraft(event.target.value);
          const next = Number(event.target.value);
          if (event.target.value.trim() === "" || !Number.isSafeInteger(next)) return;
          onChange(Math.min(Math.max(next, 0), ceiling));
        }}
        onBlur={() => setDraft(null)}
      />
    </Field>
  );
}

/**
 * The two numbers that shape a search's results: how many come back, and how
 * much of each one's text is kept.
 *
 * Drawn wherever a search is configured — the conversation, and a subagent role
 * that runs its own — because they are answers about this conversation's
 * context budget rather than about the installation. Both read 0 as "no limit",
 * which is why neither has a switch beside it: the off state is a value the
 * field can already hold, and a switch would be a second way to say it.
 */
export function SearchResultShapingFields({
  maxResults,
  compressionCutoff,
  onChangeMaxResults,
  onChangeCompressionCutoff
}: {
  maxResults: number;
  compressionCutoff: number;
  onChangeMaxResults: (next: number) => void;
  onChangeCompressionCutoff: (next: number) => void;
}) {
  const { t } = useI18n();
  return <>
    <NumberField
      label={t("结果数", "Result count")}
      hint={t(
        "一次检索最多要回几条结果，0 表示不设限。只对目录提供商生效；原生后端的检索深度由那个模型自己决定。",
        "How many results one search asks for; 0 asks for no cap. Only catalog providers read it — a native backend decides its own search depth."
      )}
      value={maxResults}
      ceiling={SEARCH_MAX_RESULTS_CEILING}
      onChange={onChangeMaxResults}
    />
    <NumberField
      label={t("结果压缩", "Result compression")}
      hint={t(
        "整次调用的 token 预算，逐条平分后截断；0 表示不设限，上游给多长正文就进多长——一次抓取可能因此占掉一整个上下文窗口。",
        "A whole-call token budget, split evenly across the results and truncated to it. 0 means no limit: whatever the upstream returns goes in verbatim, so one fetch can take a whole context window."
      )}
      value={compressionCutoff}
      ceiling={SEARCH_COMPRESSION_CUTOFF_CEILING}
      onChange={onChangeCompressionCutoff}
    />
  </>;
}

import type { SecurityLevel } from "../types";
import type { TranslationFunction } from "../i18n";

/**
 * Every security level, in the order they are offered, least to most permissive.
 *
 * One list, one order, one set of labels. The composer menu, the preset editor
 * and any future surface all read this: a level that is added here but forgotten
 * elsewhere would otherwise be shown under a neighbour's name.
 */
export const SECURITY_LEVEL_OPTIONS: readonly SecurityLevel[] = [
  "request_approval",
  "allow_edits",
  "plan",
  "full_access"
];

/**
 * Exhaustive on purpose. A `switch` that returns from every arm makes a new
 * level a compile error here instead of silently falling into the catch-all —
 * which, with the old nested ternaries, labelled the most restrictive mode
 * "Full access".
 */
export function securityLevelLabel(level: SecurityLevel, t: TranslationFunction): string {
  switch (level) {
    case "request_approval":
      return t("手动", "Manual");
    case "allow_edits":
      return t("允许编辑", "Accept edits");
    case "plan":
      return t("计划模式", "Plan mode");
    case "full_access":
      return t("完全访问", "Full access");
  }
}

export function securityLevelDescription(level: SecurityLevel, t: TranslationFunction): string {
  switch (level) {
    case "request_approval":
      return t("写入与高风险操作先询问", "Writes and risky calls ask first");
    case "allow_edits":
      return t(
        "可信目录内读写免提示",
        "Reads and writes inside trusted directories run without prompts"
      );
    case "plan":
      return t(
        "只探索与撰写计划，批准前不改动任何文件",
        "Explores and writes a plan only; nothing changes until you approve it"
      );
    case "full_access":
      return t(
        "大多数已验证调用免提示；强制确认仍保留",
        "Most validated calls run without prompts; mandatory confirmations remain"
      );
  }
}

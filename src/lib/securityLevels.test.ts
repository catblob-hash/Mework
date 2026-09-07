import { describe, expect, it } from "vitest";
import { translate } from "../i18n";
import type { SecurityLevel } from "../types";
import {
  SECURITY_LEVEL_OPTIONS,
  securityLevelDescription,
  securityLevelLabel
} from "./securityLevels";

const zh = (zhCn: string, enUs: string, parameters?: Record<string, string | number>) =>
  translate("zh-CN", zhCn, enUs, parameters);
const en = (zhCn: string, enUs: string, parameters?: Record<string, string | number>) =>
  translate("en-US", zhCn, enUs, parameters);

describe("securityLevels", () => {
  it("orders the levels from most to least supervised, with plan before full access", () => {
    expect(SECURITY_LEVEL_OPTIONS).toEqual([
      "request_approval",
      "allow_edits",
      "plan",
      "full_access"
    ]);
  });

  it("names every level in both languages", () => {
    expect(SECURITY_LEVEL_OPTIONS.map((level) => securityLevelLabel(level, zh)))
      .toEqual(["手动", "允许编辑", "计划模式", "完全访问"]);
    expect(SECURITY_LEVEL_OPTIONS.map((level) => securityLevelLabel(level, en)))
      .toEqual(["Manual", "Accept edits", "Plan mode", "Full access"]);
  });

  it("gives every level a distinct label and description", () => {
    const labels = SECURITY_LEVEL_OPTIONS.map((level) => securityLevelLabel(level, zh));
    const descriptions = SECURITY_LEVEL_OPTIONS.map(
      (level) => securityLevelDescription(level, zh)
    );

    // The old catch-all ternary labelled the most restrictive level "完全访问";
    // an exhaustive switch is what keeps a fourth level from doing that again.
    expect(new Set(labels).size).toBe(SECURITY_LEVEL_OPTIONS.length);
    expect(new Set(descriptions).size).toBe(SECURITY_LEVEL_OPTIONS.length);
    expect(descriptions.every((text) => text.length > 0)).toBe(true);
  });

  it("says plan mode changes nothing until the plan is approved", () => {
    const level: SecurityLevel = "plan";
    expect(securityLevelDescription(level, zh)).toContain("批准");
    expect(securityLevelDescription(level, en)).toContain("approve");
  });
});

import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import {
  configureI18n,
  interpolateTranslation,
  resolveApplicationLanguage,
  useI18n
} from "./i18n";

afterEach(() => configureI18n("zh-CN"));

describe("application language resolution", () => {
  it("maps every Chinese system locale to zh-CN", () => {
    expect(resolveApplicationLanguage("auto", "zh")).toBe("zh-CN");
    expect(resolveApplicationLanguage("auto", "zh-TW")).toBe("zh-CN");
    expect(resolveApplicationLanguage("auto", "zh-HK")).toBe("zh-CN");
    expect(resolveApplicationLanguage("auto", "zh-Hant-TW")).toBe("zh-CN");
    expect(resolveApplicationLanguage("auto", "ZH_cn")).toBe("zh-CN");
    expect(resolveApplicationLanguage("auto", "yue-Hant")).toBe("zh-CN");
    expect(resolveApplicationLanguage("auto", "cmn-Hans")).toBe("zh-CN");
    expect(resolveApplicationLanguage("auto", "gan")).toBe("zh-CN");
    expect(resolveApplicationLanguage("auto", "cdo-Latn")).toBe("zh-CN");
    expect(resolveApplicationLanguage("auto", "lzh-Hant")).toBe("zh-CN");
  });

  it("uses English for every unsupported automatic locale", () => {
    expect(resolveApplicationLanguage("auto", "en-GB")).toBe("en-US");
    expect(resolveApplicationLanguage("auto", "ja-JP")).toBe("en-US");
    expect(resolveApplicationLanguage("auto", "fr-FR")).toBe("en-US");
    expect(resolveApplicationLanguage("auto", "")).toBe("en-US");
  });

  it("does not replace an explicit application language", () => {
    expect(resolveApplicationLanguage("zh-CN", "en-US")).toBe("zh-CN");
    expect(resolveApplicationLanguage("en-US", "zh-TW")).toBe("en-US");
  });
});

describe("translation store", () => {
  it("interpolates known parameters and preserves unknown placeholders", () => {
    expect(interpolateTranslation("已保存 {count} 项，{missing}", { count: 3 }))
      .toBe("已保存 3 项，{missing}");
  });

  it("updates useI18n subscribers while defaulting unconfigured tests to Chinese", () => {
    configureI18n("zh-CN");
    const { result } = renderHook(() => useI18n());
    expect(result.current.resolvedLanguage).toBe("zh-CN");
    expect(result.current.t("保存", "Save")).toBe("保存");

    act(() => configureI18n("en-US"));

    expect(result.current.resolvedLanguage).toBe("en-US");
    expect(result.current.t("保存 {count}", "Save {count}", { count: 2 })).toBe("Save 2");
  });
});

import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { GlobalSettingsNavigation } from "./GlobalSettingsNavigation";

afterEach(() => configureI18n("zh-CN"));

describe("GlobalSettingsNavigation", () => {
  it("renders the grouped information architecture and updates immediately when the language changes", () => {
    configureI18n("auto", "en-US");
    const onSelect = vi.fn();
    render(<GlobalSettingsNavigation view="appearance" onSelect={onSelect} />);

    const navigation = screen.getByRole("navigation", { name: "Global settings categories" });
    // The fixed order is the navigation specification.
    expect(within(navigation).getAllByRole("button").map((button) => button.textContent)).toEqual([
      "Model providers",
      "Search providers",
      "MCP",
      "Skills",
      "Appearance",
      "Conversation presets",
      "Keyboard shortcuts",
      "Dependencies",
      "Updates"
    ]);
    // Group titles are non-interactive and cannot collapse.
    expect(
      Array.from(navigation.querySelectorAll(".settings-nav__group-title")).map((node) => node.textContent)
    ).toEqual(["Providers", "Tools", "Preferences", "Efficiency", "System"]);
    expect(navigation).toHaveTextContent("Tools");
    expect(navigation).toHaveTextContent("Preferences");
    expect(navigation).toHaveTextContent("Efficiency");
    expect(navigation).toHaveTextContent("System");
    expect(navigation).not.toHaveTextContent("Capabilities & connections");
    expect(within(navigation).queryByRole("button", { name: "General" })).toBeNull();

    expect(within(navigation).getByRole("button", { name: "Appearance" })).toHaveClass("settings-nav__item--active");
    fireEvent.click(within(navigation).getByRole("button", { name: "Model providers" }));
    expect(onSelect).toHaveBeenCalledWith("providers");
    // Search providers are their own column, not a section of the model page.
    fireEvent.click(within(navigation).getByRole("button", { name: "Search providers" }));
    expect(onSelect).toHaveBeenCalledWith("search_providers");
    // Skills and MCP are dedicated pages, not conversation-preset redirects.
    fireEvent.click(within(navigation).getByRole("button", { name: "MCP" }));
    expect(onSelect).toHaveBeenCalledWith("mcp");
    fireEvent.click(within(navigation).getByRole("button", { name: "Skills" }));
    expect(onSelect).toHaveBeenCalledWith("skills");

    act(() => configureI18n("zh-CN"));

    const localizedNavigation = screen.getByRole("navigation", { name: "全局设置分类" });
    expect(within(localizedNavigation).getAllByRole("button").map((button) => button.textContent)).toEqual([
      "模型提供商",
      "搜索提供商",
      "MCP",
      "技能",
      "外观",
      "对话预设",
      "快捷键",
      "环境依赖",
      "版本更新"
    ]);
    expect(
      Array.from(localizedNavigation.querySelectorAll(".settings-nav__group-title")).map((node) => node.textContent)
    ).toEqual(["提供商", "工具", "偏好", "效率", "系统"]);
    expect(localizedNavigation).toHaveTextContent("工具");
    expect(localizedNavigation).not.toHaveTextContent("能力与连接");
  });
});

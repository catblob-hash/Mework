import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../../i18n";
import type {
  EnvironmentToolDefinition,
  EnvironmentToolSnapshot
} from "../../types";

const dependencyMocks = vi.hoisted(() => ({
  environmentToolSnapshots: vi.fn<() => Promise<EnvironmentToolSnapshot[]>>(),
  revealEnvironmentTool: vi.fn<(executable: string) => Promise<void>>()
}));

vi.mock("../../lib/backend", () => ({
  hasBackendRuntime: () => true
}));
vi.mock("../../lib/runtime", () => dependencyMocks);

import { DependencySettings } from ".";

const installedBuiltin: EnvironmentToolSnapshot = {
  name: "Node",
  executable: "node",
  path: "C:\\Tools\\node.exe",
  version: "22.14.0",
  error: "",
  description: "JavaScript runtime",
  repoUrl: "https://github.com/nodejs/node",
  homepage: "https://nodejs.org",
  builtin: true
};

const missingCustom: EnvironmentToolSnapshot = {
  name: "CustomCli",
  executable: "custom-cli",
  path: "",
  version: "",
  error: "",
  description: "Custom command line tool",
  repoUrl: "",
  homepage: "",
  builtin: false
};

const customDefinition: EnvironmentToolDefinition = {
  name: "CustomCli",
  executable: "custom-cli",
  versionArgs: []
};

afterEach(() => configureI18n("zh-CN"));

describe("DependencySettings", () => {
  beforeEach(() => {
    configureI18n("zh-CN");
    dependencyMocks.environmentToolSnapshots.mockReset().mockResolvedValue([
      installedBuiltin,
      missingCustom
    ]);
    dependencyMocks.revealEnvironmentTool.mockReset().mockResolvedValue(undefined);
  });

  it("shows installed details and a detection-only missing state", async () => {
    render(<DependencySettings tools={[customDefinition]} onChange={vi.fn()} />);

    expect(await screen.findByText("v22.14.0")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "打开所在目录" })).toBeInTheDocument();
    expect(screen.getByText("未检测到")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /安装/ })).not.toBeInTheDocument();
  });

  it("only offers deletion for user-added tools", async () => {
    render(<DependencySettings tools={[customDefinition]} onChange={vi.fn()} />);
    await screen.findByText("CustomCli");

    expect(screen.queryByRole("button", { name: "删除 Node" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "删除 CustomCli" })).toBeInTheDocument();
  });

  it("rejects a builtin name case-insensitively", async () => {
    const user = userEvent.setup();
    render(<DependencySettings tools={[customDefinition]} onChange={vi.fn()} />);
    await screen.findByText("Node");
    await user.click(screen.getByRole("button", { name: "添加工具" }));

    const dialog = screen.getByRole("dialog");
    await user.type(within(dialog).getByLabelText("名称"), "node");
    await user.type(within(dialog).getByLabelText("可执行文件名"), "another-node");

    expect(within(dialog).getByText("已有同名工具。")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "添加工具" })).toBeDisabled();
  });

  it("rejects names outside the identifier pattern", async () => {
    const user = userEvent.setup();
    render(<DependencySettings tools={[]} onChange={vi.fn()} />);
    await waitFor(() => expect(dependencyMocks.environmentToolSnapshots).toHaveBeenCalled());
    await user.click(screen.getByRole("button", { name: "添加工具" }));

    const dialog = screen.getByRole("dialog");
    await user.type(within(dialog).getByLabelText("名称"), "bad name");
    await user.type(within(dialog).getByLabelText("可执行文件名"), "bad-name");

    expect(within(dialog).getByText("名称必须以字母开头，并且只能包含字母、数字、下划线或连字符。")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "添加工具" })).toBeDisabled();
  });
});

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { questionsFromInput } from "../lib/orchestration";
import type { ContextItem, ToolContext, ToolDescriptor } from "../types";

import { ConversationTemplateEditor } from "./ConversationTemplateEditor";

configureI18n("zh-CN");

/** A captured card: already executed once, and attested for the source conversation. */
const recordedCard: ToolContext = {
  id: "ctx_tool",
  kind: "tool",
  toolName: "ls",
  input: { path: "." },
  result: { success: true, output: "src", images: [], executedAt: "2026-09-15T00:00:01.000Z", durationMs: 4 },
  attestation: "attested-for-the-source-conversation",
  createdAt: "2026-09-15T00:00:01.000Z"
};

const body: ContextItem[] = [
  { id: "ctx_user", kind: "user", content: "请审查这段改动", images: [], createdAt: "2026-09-15T00:00:00.000Z" },
  recordedCard,
  { id: "ctx_reply", kind: "assistant", content: "看完了。", interrupted: false, sources: [], createdAt: "2026-09-15T00:00:02.000Z" }
];

const tools: ToolDescriptor[] = [
  {
    name: "ls",
    label: "列出目录",
    description: "",
    category: "filesystem",
    dangerous: false,
    parameters: [{ name: "path", label: "路径", type: "string", required: true }]
  },
  {
    name: "bash",
    label: "运行命令",
    description: "",
    category: "shell",
    dangerous: true,
    parameters: [{ name: "command", label: "命令", type: "string", required: true }]
  }
];

function open(overrides: Partial<Parameters<typeof ConversationTemplateEditor>[0]> = {}) {
  const onSave = vi.fn().mockResolvedValue(undefined);
  const onEnableTools = vi.fn();
  const { container } = render(
    <ConversationTemplateEditor
      templateId="template_review"
      contexts={body}
      tools={tools}
      enabledTools={["ls", "bash"]}
      onSave={onSave}
      onEnableTools={onEnableTools}
      {...overrides}
    />
  );
  return { page: container.querySelector(".template-editor") as HTMLElement, onSave, onEnableTools };
}

/** The page's own save, as opposed to the 保存 an inline card editor carries. */
function pageSave(page: HTMLElement): HTMLElement {
  const actions = page.querySelector(".template-editor__actions") as HTMLElement;
  return within(actions).getByRole("button", { name: /^保存模板|^正在保存/ });
}

describe("ConversationTemplateEditor", () => {
  it("is a page with nothing but its own save, and saves nothing until something changes", async () => {
    const user = userEvent.setup();
    const { page, onSave } = open();

    // The chrome belongs to whoever embeds this — a preset's page or a role's
    // window — so the editor draws no dialog and nothing here closes it.
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(within(page).queryByRole("button", { name: "关闭" })).toBeNull();

    const save = pageSave(page);
    expect(save).toBeDisabled();

    await user.click(within(page).getAllByRole("button", { name: "删除上下文" })[0]);
    expect(save).toBeEnabled();
    await user.click(save);
    await waitFor(() => expect(onSave).toHaveBeenCalledWith([body[1], body[2]]));
  });

  it("offers a call to place, and no way to run one", async () => {
    const user = userEvent.setup();
    const { page } = open();

    expect(within(page).getByRole("button", { name: /^编辑工具调用/ })).toBeInTheDocument();
    expect(within(page).getByRole("button", { name: /^删除工具调用/ })).toBeInTheDocument();

    await user.pointer({ keys: "[MouseRight]", target: page.querySelector(".context-stream")! });
    const menu = await screen.findByRole("menu");
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "系统提示词",
      "用户输入",
      "思考字段",
      "工具调用",
      "模型回复"
    ]);

    // `ls` is rerunnable on a real timeline, which is exactly why its absence
    // here proves the button is withheld by the surface and not by the tool.
    await user.keyboard("{Escape}");
    await user.click(within(page).getByRole("button", { name: /^编辑工具调用/ }));
    const card = page.querySelector(".inline-tool-editor") as HTMLElement;
    expect(within(card).getByRole("button", { name: "保存" })).toBeInTheDocument();
    expect(within(card).queryByRole("button", { name: "重新执行" })).not.toBeInTheDocument();
    expect(within(card).queryByRole("button", { name: "执行并添加" })).not.toBeInTheDocument();
  });

  it("offers only the tools its owner enables as calls to place", async () => {
    const user = userEvent.setup();
    // The owner hands the model `ls` and nothing else, so a `bash` card written
    // here would be prose the model can read and never act on.
    const { page } = open({ enabledTools: ["ls"] });

    await user.pointer({ keys: "[MouseRight]", target: page.querySelector(".context-stream")! });
    await user.click(within(await screen.findByRole("menu")).getByRole("menuitem", { name: /工具调用/ }));

    const groups = screen.getByRole("menu", { name: "工具调用" });
    expect(within(groups).queryByRole("menuitem", { name: "Shell" })).not.toBeInTheDocument();
    await user.click(within(groups).getByRole("menuitem", { name: /文件与搜索/ }));

    const offered = screen.getByRole("menu", { name: "文件与搜索" });
    expect(within(offered).getByRole("menuitem", { name: "列出目录" })).toBeInTheDocument();
    expect(within(offered).queryByRole("menuitem", { name: "运行命令" })).not.toBeInTheDocument();
  });

  it("asks about a call the owner no longer enables, and repairs it on request", async () => {
    const user = userEvent.setup();
    // The switch was turned off after the body was written: the menu could not
    // have placed this card today, and only saving has both halves in view.
    const { page, onSave, onEnableTools } = open({ enabledTools: ["bash"] });

    await user.click(within(page).getAllByRole("button", { name: "删除上下文" })[0]);
    await user.click(pageSave(page));
    // The question comes first: nothing is written until it is answered.
    expect(onSave).not.toHaveBeenCalled();

    const dialog = screen.getByRole("dialog", { name: "模板里有未启用的工具" });
    expect(within(dialog).getByText("列出目录")).toBeInTheDocument();
    expect(within(dialog).getByText("ls")).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "自动启用所需工具" }));
    // Exactly the missing names, and the save goes through in the same breath.
    expect(onEnableTools).toHaveBeenCalledWith(["ls"]);
    await waitFor(() => expect(onSave).toHaveBeenCalledWith([body[1], body[2]]));
  });

  it("saves a body with an unenabled call as it is when told to", async () => {
    const user = userEvent.setup();
    const { page, onSave, onEnableTools } = open({ enabledTools: ["bash"] });

    await user.click(within(page).getAllByRole("button", { name: "删除上下文" })[0]);
    await user.click(pageSave(page));
    const dialog = screen.getByRole("dialog", { name: "模板里有未启用的工具" });
    await user.click(within(dialog).getByRole("button", { name: "正常保存" }));

    await waitFor(() => expect(onSave).toHaveBeenCalledWith([body[1], body[2]]));
    // Turning a tool off is the user's call, and saying "save anyway" is not a
    // request to turn it back on.
    expect(onEnableTools).not.toHaveBeenCalled();
  });

  it("withholds every mutation affordance from a body it does not own", () => {
    // What the rail's read-only preset entries draw: the same body, and nothing
    // to change it with.
    const { page } = open({ editable: false });

    expect(page.querySelector(".template-editor__actions")).toBeNull();
    expect(within(page).queryByRole("button", { name: "删除上下文" })).not.toBeInTheDocument();
    expect(within(page).queryByRole("button", { name: /^编辑工具调用/ })).not.toBeInTheDocument();
  });

  it("rewrites a tool card without claiming the source conversation's credential", async () => {
    const user = userEvent.setup();
    const { page, onSave } = open();

    await user.click(within(page).getByRole("button", { name: /^编辑工具调用/ }));
    const card = page.querySelector(".inline-tool-editor") as HTMLElement;
    const path = within(card).getByRole("textbox", { name: "路径 *" });
    await user.clear(path);
    await user.type(path, "src");
    const result = within(card).getByRole("textbox", { name: "返回值" });
    await user.clear(result);
    await user.type(result, "App.tsx");
    await user.click(within(card).getByRole("button", { name: "保存" }));

    await user.click(pageSave(page));
    await waitFor(() => expect(onSave).toHaveBeenCalled());
    expect(onSave.mock.calls[0][0][1]).toEqual({
      ...recordedCard,
      input: { path: "src" },
      // The host owns everything else about a result and normalizes the body on
      // save, but the draft must not ship a token bound to another conversation.
      attestation: undefined,
      requestedInput: undefined,
      result: { ...recordedCard.result, output: "App.tsx" }
    });
  });

  it("places a call written out by hand, with no result the host did not mint", async () => {
    const user = userEvent.setup();
    const { page, onSave } = open();

    await user.pointer({ keys: "[MouseRight]", target: page.querySelector(".context-stream")! });
    const menu = await screen.findByRole("menu");
    await user.click(within(menu).getByRole("menuitem", { name: /工具调用/ }));
    await user.click(await screen.findByRole("menuitem", { name: "Shell" }));
    await user.click(await screen.findByRole("menuitem", { name: "运行命令" }));

    const card = page.querySelector(".inline-tool-editor") as HTMLElement;
    await user.type(within(card).getByRole("textbox", { name: "命令 *" }), "npm test");
    await user.type(within(card).getByRole("textbox", { name: "返回值" }), "全部通过");
    expect(within(card).queryByRole("button", { name: "执行并添加" })).not.toBeInTheDocument();
    await user.click(within(card).getByRole("button", { name: "保存" }));

    await user.click(pageSave(page));
    await waitFor(() => expect(onSave).toHaveBeenCalled());
    const saved = onSave.mock.calls[0][0];
    // Placed where the menu was opened — after the last message, since that is
    // what the stream's trailing hint stands for.
    expect(saved).toHaveLength(body.length + 1);
    const placed = saved[saved.length - 1];
    expect(placed).toMatchObject({
      kind: "tool",
      toolName: "bash",
      input: { command: "npm test" },
      result: { success: true, output: "全部通过", images: [], executedAt: "", durationMs: 0 }
    });
    expect(placed.attestation).toBeUndefined();
    expect(placed.result.diff).toBeUndefined();
  });

  it("edits prose in place and hands the whole body over on save", async () => {
    const user = userEvent.setup();
    const { page, onSave } = open();

    await user.click(within(page).getAllByRole("button", { name: "编辑上下文" })[0]);
    const field = within(page).getByRole("textbox", { name: "用户输入" });
    await user.clear(field);
    await user.type(field, "改过的请求");
    // The card's own Save closes the inline editor; the page's Save is the one
    // that hands the body to the host.
    const card = page.querySelector(".inline-text-editor") as HTMLElement;
    await user.click(within(card).getByRole("button", { name: "保存" }));
    await user.click(pageSave(page));

    await waitFor(() => expect(onSave).toHaveBeenCalled());
    expect(onSave.mock.calls[0][0]).toEqual([
      { ...body[0], content: "改过的请求" },
      body[1],
      body[2]
    ]);
  });

  it("rewrites a question card and the answer that settled it", async () => {
    const user = userEvent.setup();
    const question: ToolContext = {
      id: "ctx_ask",
      kind: "tool",
      toolName: "ask_user",
      input: {
        questions: [{
          question: "先修哪一处？",
          header: "范围",
          multiSelect: false,
          options: [
            { label: "内核", description: "先动内核" },
            { label: "界面", description: "先动界面" }
          ]
        }]
      },
      result: { success: true, output: "", images: [], executedAt: "2026-09-15T00:00:03.000Z", durationMs: 1 },
      attestation: "attested-for-the-source-conversation",
      createdAt: "2026-09-15T00:00:03.000Z"
    };
    const answer: ContextItem = {
      id: "ctx_answer",
      kind: "user",
      content: 'User has answered your questions: "先修哪一处？"="内核"',
      images: [],
      createdAt: "2026-09-15T00:00:04.000Z"
    };
    const { page, onSave } = open({ contexts: [question, answer] });

    await user.click(within(page).getByRole("button", { name: "编辑提问与回答" }));
    // In place, like every other editor on this surface.
    expect(screen.queryAllByRole("dialog")).toHaveLength(0);
    await user.clear(screen.getByLabelText("第 1 题问题内容"));
    await user.type(screen.getByLabelText("第 1 题问题内容"), "先修哪个模块？");
    await user.clear(screen.getByLabelText("第 1 题回答"));
    await user.type(screen.getByLabelText("第 1 题回答"), "界面");
    const card = page.querySelector(".question-editor") as HTMLElement;
    await user.click(within(card).getByRole("button", { name: "保存" }));

    await user.click(pageSave(page));
    await waitFor(() => expect(onSave).toHaveBeenCalled());
    const [saved] = onSave.mock.calls[0] as [ContextItem[]];
    const savedQuestion = saved[0] as ToolContext;
    expect(questionsFromInput(savedQuestion.input)[0].question).toBe("先修哪个模块？");
    expect(savedQuestion.attestation).toBeUndefined();
    expect((saved[1] as { content: string }).content).toContain('"界面"');
  });

  it("reports a refused save beside the button and keeps the draft to fix", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn().mockRejectedValue(new Error("带有子代理记录的工具卡不能手动编辑"));
    const { container } = render(
      <ConversationTemplateEditor
        templateId="template_review"
        contexts={body}
        tools={tools}
        enabledTools={["ls", "bash"]}
        onSave={onSave}
      />
    );
    const page = container.querySelector(".template-editor") as HTMLElement;

    await user.click(within(page).getAllByRole("button", { name: "删除上下文" })[0]);
    await user.click(pageSave(page));

    expect(await within(page).findByRole("alert")).toHaveTextContent("不能手动编辑");
    // Still holding what the user did: a refusal is something to fix.
    expect(pageSave(page)).toBeEnabled();
  });
});

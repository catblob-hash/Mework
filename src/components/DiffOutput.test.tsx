import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import {
  DiffOutput,
  parseUnifiedDiff,
  type DiffLineSelection
} from "./DiffOutput";

const sampleDiff = [
  "--- a/src/example.ts",
  "+++ b/src/example.ts",
  "@@ -2,3 +2,4 @@",
  " keep();",
  "-oldValue();",
  "+newValue();",
  "+extraValue();",
  " done();",
  ""
].join("\n");

describe("DiffOutput", () => {
  it("parses unified diff line kinds, paths, counts, and line numbers", () => {
    const parsed = parseUnifiedDiff(sampleDiff);

    expect(parsed.path).toBe("src/example.ts");
    expect(parsed.additions).toBe(2);
    expect(parsed.deletions).toBe(1);
    expect(parsed.lines.find((line) => line.kind === "deletion")).toMatchObject({
      text: "oldValue();",
      oldLineNumber: 3,
      newLineNumber: null
    });
    expect(parsed.lines.filter((line) => line.kind === "addition")).toEqual([
      expect.objectContaining({ text: "newValue();", oldLineNumber: null, newLineNumber: 3 }),
      expect.objectContaining({ text: "extraValue();", oldLineNumber: null, newLineNumber: 4 })
    ]);
  });

  it("renders an accessible read-only diff without markers or header rows", () => {
    const { container } = render(<DiffOutput value={sampleDiff} />);
    const region = screen.getByRole("region", { name: "src/example.ts 文件差异" });

    expect(within(region).getByLabelText("新增 2 行，删除 1 行")).toBeInTheDocument();
    expect(container.querySelector(".diff-output__line--addition")).toHaveTextContent("newValue();");
    expect(container.querySelector(".diff-output__line--deletion")).toHaveTextContent("oldValue();");
    expect(container.querySelector(".diff-output__marker")).toBeNull();
    expect(within(region).queryByText("--- a/src/example.ts")).not.toBeInTheDocument();
    expect(within(region).queryByText("+++ b/src/example.ts")).not.toBeInTheDocument();
    expect(within(region).queryByText("@@ -2,3 +2,4 @@")).not.toBeInTheDocument();
    expect(within(region).queryByRole("button")).not.toBeInTheDocument();
  });

  it("carries one line number per row and sizes the gutter to its widest number", () => {
    const { container } = render(
      <DiffOutput value={["--- a/wide.ts", "+++ b/wide.ts", "@@ -999,3 +999,3 @@", " keep();", "-old();", "+new();", " tail();", ""].join("\n")} />
    );
    const region = container.querySelector<HTMLElement>(".diff-output")!;
    const rows = container.querySelectorAll(".diff-output__line");

    expect(region.style.getPropertyValue("--diff-gutter")).toBe("4ch");
    for (const row of rows) {
      expect(row.querySelectorAll(".diff-output__line-number")).toHaveLength(1);
    }
    // A deletion is addressed in the old file, everything else in the new one.
    expect(container.querySelector(".diff-output__line--deletion .diff-output__line-number")).toHaveTextContent("1000");
    expect(container.querySelector(".diff-output__line--addition .diff-output__line-number")).toHaveTextContent("1000");
  });

  it("groups each run of changes with its deletions above its additions", () => {
    const { container } = render(
      <DiffOutput
        value={["--- a/x.ts", "+++ b/x.ts", "@@ -1,4 +1,4 @@", "+added-first", "-removed-after", " keep();", "+tail-add", "-tail-remove", ""].join("\n")}
      />
    );
    const changed = [...container.querySelectorAll(".diff-output__line--addition, .diff-output__line--deletion")]
      .map((row) => row.querySelector(".diff-output__content")?.textContent);

    expect(changed).toEqual(["removed-after", "added-first", "tail-remove", "tail-add"]);
  });

  it("does not confuse changed content beginning with diff header markers", () => {
    const parsed = parseUnifiedDiff("--- markers.txt\n+++ markers.txt\n@@ -1 +1 @@\n----\n++++\n");

    expect(parsed.deletions).toBe(1);
    expect(parsed.additions).toBe(1);
    expect(parsed.lines.find((line) => line.kind === "deletion")?.text).toBe("---");
    expect(parsed.lines.find((line) => line.kind === "addition")?.text).toBe("+++");
  });

  it("selects additions on RIGHT and deletions on LEFT with semantic payloads", async () => {
    const user = userEvent.setup();
    const onLineSelect = vi.fn();
    render(<DiffOutput value={sampleDiff} onLineSelect={onLineSelect} />);

    const deletionRow = screen.getByText("oldValue();").closest(".diff-output__line");
    const additionRow = screen.getByText("newValue();").closest(".diff-output__line");
    expect(deletionRow).not.toBeNull();
    expect(additionRow).not.toBeNull();
    expect(within(deletionRow as HTMLElement).queryByRole("button", {
      name: /新文件/
    })).not.toBeInTheDocument();
    expect(within(additionRow as HTMLElement).queryByRole("button", {
      name: /旧文件/
    })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", {
      name: "选择 src/example.ts 旧文件第 3 行"
    }));
    await user.click(screen.getByRole("button", {
      name: "选择 src/example.ts 新文件第 3 行"
    }));

    expect(onLineSelect).toHaveBeenNthCalledWith(1, {
      path: "src/example.ts",
      line: 3,
      side: "LEFT",
      text: "oldValue();",
      kind: "deletion"
    });
    expect(onLineSelect).toHaveBeenNthCalledWith(2, {
      path: "src/example.ts",
      line: 3,
      side: "RIGHT",
      text: "newValue();",
      kind: "addition"
    });
  });

  it("addresses a context line on the new side only, now that one column carries it", async () => {
    const user = userEvent.setup();
    const onLineSelect = vi.fn();
    render(<DiffOutput value={sampleDiff} onLineSelect={onLineSelect} />);

    expect(screen.queryByRole("button", {
      name: "选择 src/example.ts 旧文件第 2 行"
    })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", {
      name: "选择 src/example.ts 新文件第 2 行"
    }));

    expect(onLineSelect).toHaveBeenCalledWith({
      path: "src/example.ts",
      line: 2,
      side: "RIGHT",
      text: "keep();",
      kind: "context"
    });
  });

  it("supports keyboard activation and a controlled selected state", async () => {
    const user = userEvent.setup();
    const onLineSelect = vi.fn();
    const selection: DiffLineSelection = {
      path: "src/example.ts",
      line: 4,
      side: "RIGHT",
      text: "extraValue();",
      kind: "addition"
    };
    const view = render(
      <DiffOutput
        value={sampleDiff}
        selectedLine={null}
        onLineSelect={onLineSelect}
      />
    );
    const button = screen.getByRole("button", {
      name: "选择 src/example.ts 新文件第 4 行"
    });

    button.focus();
    await user.keyboard("{Enter}");
    expect(onLineSelect).toHaveBeenCalledWith(selection);
    expect(button).toHaveAttribute("aria-pressed", "false");

    view.rerender(
      <DiffOutput
        value={sampleDiff}
        selectedLine={selection}
        onLineSelect={onLineSelect}
      />
    );
    expect(screen.getByRole("button", {
      name: "选择 src/example.ts 新文件第 4 行"
    })).toHaveAttribute("aria-pressed", "true");

    await user.keyboard(" ");
    expect(onLineSelect).toHaveBeenCalledTimes(2);
  });

  it("drops file and hunk rows and keeps meta and generated omission rows non-interactive", () => {
    const contextLines = Array.from({ length: 810 }, (_, index) => ` line-${index}`);
    const largeDiff = [
      "--- a/src/large.ts",
      "+++ b/src/large.ts",
      "@@ -1,810 +1,810 @@",
      ...contextLines,
      "\\ No newline at end of file"
    ].join("\n");
    render(<DiffOutput value={largeDiff} onLineSelect={() => undefined} />);

    expect(screen.queryByText("--- a/src/large.ts")).not.toBeInTheDocument();
    expect(screen.queryByText("@@ -1,810 +1,810 @@")).not.toBeInTheDocument();
    const omissionRow = screen.getByText(/行差异未显示/).closest(".diff-output__line");
    const metaRow = screen.getByText("\\ No newline at end of file").closest(".diff-output__line");

    expect(omissionRow).not.toBeNull();
    expect(metaRow).not.toBeNull();
    for (const row of [omissionRow!, metaRow!]) {
      expect(within(row as HTMLElement).queryByRole("button")).not.toBeInTheDocument();
    }
  });

  it("does not emit a localized placeholder as a review path", () => {
    render(
      <DiffOutput
        value={"@@ -1 +1 @@\n-old\n+new"}
        onLineSelect={() => undefined}
      />
    );

    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});

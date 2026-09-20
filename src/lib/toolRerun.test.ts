import { describe, expect, it } from "vitest";
import { toolCatalog } from "../seed";
import { canRerunTool } from "./toolRerun";

const RERUNNABLE = toolCatalog.filter((tool) => canRerunTool(tool.name)).map((tool) => tool.name);

describe("canRerunTool", () => {
  it("names only tools that exist in the catalog", () => {
    expect(RERUNNABLE).toHaveLength(12);
  });

  it("refuses every tool that mutates something outside the conversation record", () => {
    for (const name of ["write", "edit", "bash", "powershell", "preview_click", "preview_fill", "preview_eval", "preview_start", "preview_stop", "preview_resize", "preview_dialog"]) {
      expect(canRerunTool(name), name).toBe(false);
    }
  });

  it("refuses tools the host classifier will not execute outside the model run loop", () => {
    for (const name of ["web_search", "web_fetch", "ask_user", "todo", "workflow", "agent_spawn", "task_wait", "box", "read_project_memory", "edit_global_memory", "mcp__example__do_thing"]) {
      expect(canRerunTool(name), name).toBe(false);
    }
  });
});

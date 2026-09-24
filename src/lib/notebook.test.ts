import { describe, expect, it } from "vitest";
import { parseNotebook } from "./notebook";

describe("parseNotebook", () => {
  it("reads nbformat 4 cells, joining line arrays", () => {
    const notebook = parseNotebook(JSON.stringify({
      nbformat: 4,
      metadata: { language_info: { name: "R" } },
      cells: [
        { cell_type: "markdown", source: ["# T\n", "x"], attachments: { "a.png": { "image/png": "AA" } } },
        { cell_type: "code", execution_count: 1, source: ["1 + 1"], outputs: [
          { output_type: "execute_result", execution_count: 1, data: { "text/plain": ["2"], "application/json": { a: 1 } } },
          { output_type: "stream", name: "stderr", text: "warn\n" }
        ] },
        { cell_type: "raw", source: "raw" },
        { cell_type: "unknown", source: "?" }
      ]
    }));
    expect(notebook.language).toBe("r");
    expect(notebook.cells).toEqual([
      { kind: "markdown", source: "# T\nx", attachments: { "a.png": { "image/png": "AA" } } },
      { kind: "code", source: "1 + 1", executionCount: 1, outputs: [
        { kind: "data", executionCount: 1, data: { "text/plain": "2", "application/json": "{\n  \"a\": 1\n}" }, result: true },
        { kind: "stream", name: "stderr", text: "warn\n" }
      ] },
      { kind: "raw", source: "raw" }
    ]);
  });

  it("flattens nbformat 3 worksheets", () => {
    const notebook = parseNotebook(JSON.stringify({
      nbformat: 3,
      worksheets: [{ cells: [
        { cell_type: "heading", level: 2, source: "Old" },
        { cell_type: "code", input: "x", prompt_number: 5, outputs: [
          { output_type: "pyout", prompt_number: 5, text: ["1"], png: "AA" },
          { output_type: "pyerr", ename: "E", evalue: "v", traceback: ["t1", "t2"] }
        ] }
      ] }]
    }));
    expect(notebook.language).toBe("python");
    expect(notebook.cells[0]).toEqual({ kind: "markdown", source: "## Old", attachments: {} });
    expect(notebook.cells[1]).toEqual({ kind: "code", source: "x", executionCount: 5, outputs: [
      { kind: "data", executionCount: 5, data: { "text/plain": "1", "image/png": "AA" }, result: true },
      { kind: "error", name: "E", value: "v", traceback: "t1\nt2" }
    ] });
  });

  it("throws on text that is not JSON, for the viewer to report", () => {
    expect(() => parseNotebook("{nope")).toThrow();
  });
});

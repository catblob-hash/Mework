/**
 * A Jupyter notebook, read into cells the viewer can draw.
 *
 * Only what a reader sees is kept: sources, execution counts, and outputs by
 * MIME type. nbformat 4 is the format in use; nbformat 3's worksheets are
 * flattened so an old notebook still opens.
 */

export type NotebookOutput =
  | { kind: "stream"; name: string; text: string }
  | { kind: "data"; executionCount: number | null; data: Record<string, string>; result: boolean }
  | { kind: "error"; name: string; value: string; traceback: string };

export type NotebookCell =
  | { kind: "markdown"; source: string; attachments: Record<string, Record<string, string>> }
  | { kind: "code"; source: string; executionCount: number | null; outputs: NotebookOutput[] }
  | { kind: "raw"; source: string };

export interface Notebook {
  language: string;
  cells: NotebookCell[];
}

type Json = Record<string, unknown>;

/** nbformat stores multi-line strings as arrays of lines, each with its own newline. */
function joined(value: unknown): string {
  if (Array.isArray(value)) return value.map((part) => (typeof part === "string" ? part : "")).join("");
  return typeof value === "string" ? value : "";
}

function record(value: unknown): Json {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Json : {};
}

function count(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function mimeBundle(value: unknown): Record<string, string> {
  const bundle: Record<string, string> = {};
  for (const [type, content] of Object.entries(record(value))) {
    bundle[type] = typeof content === "object" && content !== null && !Array.isArray(content)
      ? JSON.stringify(content, null, 2)
      : joined(content);
  }
  return bundle;
}

function outputOf(raw: unknown): NotebookOutput | null {
  const output = record(raw);
  switch (output.output_type) {
    case "stream":
      return { kind: "stream", name: typeof output.name === "string" ? output.name : "stdout", text: joined(output.text) };
    case "execute_result":
    case "display_data":
      return {
        kind: "data",
        executionCount: count(output.execution_count),
        data: mimeBundle(output.data),
        result: output.output_type === "execute_result"
      };
    // nbformat 3 kept each MIME type as its own key on the output itself.
    case "pyout":
    case "display_data_v3": {
      const { output_type: _type, prompt_number, metadata: _metadata, ...rest } = output;
      const data: Record<string, string> = {};
      const v3Types: Record<string, string> = {
        text: "text/plain", html: "text/html", png: "image/png", jpeg: "image/jpeg", svg: "image/svg+xml",
        latex: "text/latex", json: "application/json", javascript: "application/javascript"
      };
      for (const [key, value] of Object.entries(rest)) data[v3Types[key] ?? key] = joined(value);
      return { kind: "data", executionCount: count(prompt_number), data, result: true };
    }
    case "error":
    case "pyerr":
      return {
        kind: "error",
        name: typeof output.ename === "string" ? output.ename : "Error",
        value: typeof output.evalue === "string" ? output.evalue : "",
        traceback: Array.isArray(output.traceback) ? output.traceback.map(String).join("\n") : ""
      };
    default:
      return null;
  }
}

function cellOf(raw: unknown): NotebookCell | null {
  const cell = record(raw);
  const source = joined(cell.source ?? cell.input);
  switch (cell.cell_type) {
    case "markdown":
      return { kind: "markdown", source, attachments: record(cell.attachments) as Record<string, Record<string, string>> };
    case "heading": {
      const level = Math.min(6, Math.max(1, count(cell.level) ?? 1));
      return { kind: "markdown", source: `${"#".repeat(level)} ${source}`, attachments: {} };
    }
    case "code":
      return {
        kind: "code",
        source,
        executionCount: count(cell.execution_count ?? cell.prompt_number),
        outputs: (Array.isArray(cell.outputs) ? cell.outputs : [])
          .map((output) => outputOf(record(output).output_type === "display_data" && !("data" in record(output))
            ? { ...record(output), output_type: "display_data_v3" }
            : output))
          .filter((output): output is NotebookOutput => output !== null)
      };
    case "raw":
      return { kind: "raw", source };
    default:
      return null;
  }
}

export function parseNotebook(text: string): Notebook {
  const document = record(JSON.parse(text));
  const metadata = record(document.metadata);
  const language = String(
    record(metadata.kernelspec).language
      ?? record(metadata.language_info).name
      ?? metadata.language
      ?? "python"
  ).toLowerCase();
  const rawCells = Array.isArray(document.cells)
    ? document.cells
    : Array.isArray(document.worksheets)
      ? document.worksheets.flatMap((worksheet) => {
        const cells = record(worksheet).cells;
        return Array.isArray(cells) ? cells : [];
      })
      : [];
  return {
    language,
    cells: rawCells.map(cellOf).filter((cell): cell is NotebookCell => cell !== null)
  };
}

/** The first of these a bundle has is the one shown: the richest form the viewer can draw. */
export const OUTPUT_PREFERENCE = [
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "image/svg+xml",
  "text/html",
  "text/markdown",
  "text/latex",
  "application/json",
  "text/plain"
];

import { describe, expect, it } from "vitest";
import { parseAnsi } from "./ansi";

describe("parseAnsi", () => {
  it("turns colour codes into runs and drops every other control sequence", () => {
    const runs = parseAnsi("\u001b[1;31mError\u001b[0m: \u001b[32mok\u001b[39m done\u001b[2K");
    expect(runs.map((run) => [run.text, run.foreground, run.bold])).toEqual([
      ["Error", 1, true],
      [": ", null, false],
      ["ok", 2, false],
      [" done", null, false]
    ]);
  });

  it("reads bright, 256-colour and true-colour codes", () => {
    const [bright, cube, grey, truecolour] = parseAnsi("\u001b[94ma\u001b[38;5;196mb\u001b[38;5;244mc\u001b[48;2;1;2;3md");
    expect(bright.foreground).toBe(12);
    expect(cube.foreground).toBe("rgb(255 0 0)");
    expect(grey.foreground).toBe("rgb(128 128 128)");
    expect(truecolour.background).toBe("rgb(1 2 3)");
  });

  it("leaves plain text as a single run", () => {
    expect(parseAnsi("plain")).toEqual([
      { text: "plain", foreground: null, background: null, bold: false, italic: false, underline: false }
    ]);
  });
});

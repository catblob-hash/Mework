/**
 * Terminal colour codes, read into styled runs.
 *
 * Tracebacks and progress output captured from a terminal — Jupyter's above all —
 * carry SGR escape sequences. Shown raw they are noise (`[0;31m`); stripped they
 * lose the colour that says which frame raised. Only SGR is interpreted; every
 * other control sequence is removed.
 */

export interface AnsiRun {
  text: string;
  /** 0–15 for the named colours, or a CSS colour for 256-colour and true-colour codes. */
  foreground: number | string | null;
  background: number | string | null;
  bold: boolean;
  italic: boolean;
  underline: boolean;
}

const ESCAPE = /\u001b\[([0-9;]*)([A-Za-z])|\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)|\u001b[()][A-Za-z0-9]|\u001b[=>]/g;

/** The 6×6×6 cube and grey ramp of the 256-colour palette, above the 16 named colours. */
function extendedColour(index: number): number | string | null {
  if (index < 16) return index;
  if (index < 232) {
    const value = index - 16;
    const level = (component: number) => (component === 0 ? 0 : 55 + component * 40);
    return `rgb(${level(Math.floor(value / 36))} ${level(Math.floor(value / 6) % 6)} ${level(value % 6)})`;
  }
  if (index < 256) {
    const grey = 8 + (index - 232) * 10;
    return `rgb(${grey} ${grey} ${grey})`;
  }
  return null;
}

/** `input` with every escape sequence removed, colours included. */
export function stripAnsi(input: string): string {
  return input.replace(ESCAPE, "");
}

export function parseAnsi(input: string): AnsiRun[] {
  const runs: AnsiRun[] = [];
  let state: Omit<AnsiRun, "text"> = {
    foreground: null, background: null, bold: false, italic: false, underline: false
  };
  let cursor = 0;
  const push = (text: string) => {
    if (!text) return;
    const last = runs[runs.length - 1];
    if (last && last.foreground === state.foreground && last.background === state.background
      && last.bold === state.bold && last.italic === state.italic && last.underline === state.underline) {
      last.text += text;
    } else {
      runs.push({ text, ...state });
    }
  };
  for (const match of input.matchAll(ESCAPE)) {
    push(input.slice(cursor, match.index));
    cursor = (match.index ?? 0) + match[0].length;
    if (match[2] !== "m") continue;
    const codes = (match[1] || "0").split(";").map((code) => Number.parseInt(code || "0", 10));
    for (let index = 0; index < codes.length; index += 1) {
      const code = codes[index];
      if (code === 0) state = { foreground: null, background: null, bold: false, italic: false, underline: false };
      else if (code === 1) state = { ...state, bold: true };
      else if (code === 3) state = { ...state, italic: true };
      else if (code === 4) state = { ...state, underline: true };
      else if (code === 22) state = { ...state, bold: false };
      else if (code === 23) state = { ...state, italic: false };
      else if (code === 24) state = { ...state, underline: false };
      else if (code >= 30 && code <= 37) state = { ...state, foreground: code - 30 };
      else if (code >= 90 && code <= 97) state = { ...state, foreground: code - 90 + 8 };
      else if (code === 39) state = { ...state, foreground: null };
      else if (code >= 40 && code <= 47) state = { ...state, background: code - 40 };
      else if (code >= 100 && code <= 107) state = { ...state, background: code - 100 + 8 };
      else if (code === 49) state = { ...state, background: null };
      else if (code === 38 || code === 48) {
        const key = code === 38 ? "foreground" : "background";
        if (codes[index + 1] === 5) {
          state = { ...state, [key]: extendedColour(codes[index + 2] ?? -1) };
          index += 2;
        } else if (codes[index + 1] === 2) {
          const [red, green, blue] = codes.slice(index + 2, index + 5).map((value) => Math.max(0, Math.min(255, value || 0)));
          state = { ...state, [key]: `rgb(${red} ${green} ${blue})` };
          index += 4;
        }
      }
    }
  }
  push(input.slice(cursor));
  return runs;
}

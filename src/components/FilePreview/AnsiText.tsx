import { useMemo } from "react";
import type { CSSProperties } from "react";
import { parseAnsi } from "../../lib/ansi";

/** Text captured from a terminal, with its colours; plain text passes through as one run. */
export function AnsiText({ text }: { text: string }) {
  const runs = useMemo(() => parseAnsi(text), [text]);
  return (
    <>
      {runs.map((run, index) => {
        const classes: string[] = [];
        const style: CSSProperties = {};
        if (typeof run.foreground === "number") classes.push(`ansi-fg-${run.foreground}`);
        else if (run.foreground) style.color = run.foreground;
        if (typeof run.background === "number") classes.push(`ansi-bg-${run.background}`);
        else if (run.background) style.backgroundColor = run.background;
        if (run.bold) classes.push("ansi-bold");
        if (run.italic) classes.push("ansi-italic");
        if (run.underline) classes.push("ansi-underline");
        if (!classes.length && !style.color && !style.backgroundColor) return run.text;
        return (
          <span key={index} className={classes.join(" ") || undefined} style={style.color || style.backgroundColor ? style : undefined}>
            {run.text}
          </span>
        );
      })}
    </>
  );
}

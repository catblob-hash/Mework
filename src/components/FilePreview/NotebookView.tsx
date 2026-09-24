import { useMemo } from "react";
import type { MouseEvent } from "react";
import { useI18n } from "../../i18n";
import { fenceLanguage } from "../../lib/fileViewers";
import { OUTPUT_PREFERENCE, parseNotebook } from "../../lib/notebook";
import type { NotebookCell, NotebookOutput } from "../../lib/notebook";
import { CodeTokens, splitCodeLines } from "../CodeBlock";
import { highlightCodeLines } from "../../lib/codeHighlight";
import { MarkdownContent } from "../MarkdownContent";
import { AnsiText } from "./AnsiText";
import { HtmlPreview } from "./HtmlPreview";
import type { HtmlPreviewResources } from "./HtmlPreview";
import { textDataUrl } from "./format";

/** Outputs carry their own pictures; a notebook's HTML never needs the workspace. */
const NO_RESOURCES: HtmlPreviewResources = {
  readText: async () => null,
  readImage: async () => null
};

function NotebookCode({ source, language }: { source: string; language: string | null }) {
  const lines = useMemo(() => splitCodeLines(source), [source]);
  const tokens = useMemo(() => (language ? highlightCodeLines(language, lines) : null), [language, lines]);
  return (
    <pre className="notebook__code">
      <code>
        {lines.map((line, index) => (
          <span key={index}>
            <CodeTokens tokens={tokens?.[index]} fallback={line} />
            {index < lines.length - 1 ? "\n" : null}
          </span>
        ))}
      </code>
    </pre>
  );
}

function OutputData({ data }: { data: Record<string, string> }) {
  const type = OUTPUT_PREFERENCE.find((candidate) => candidate in data) ?? Object.keys(data)[0];
  if (!type) return null;
  const value = data[type];
  if (type.startsWith("image/")) {
    const source = type === "image/svg+xml"
      ? textDataUrl(value, type)
      : `data:${type};base64,${value.replace(/\s+/g, "")}`;
    return <img className="notebook__image" src={source} alt="" />;
  }
  if (type === "text/html") {
    return <HtmlPreview path="" content={value} resources={NO_RESOURCES} onOpenFile={() => undefined} inline />;
  }
  if (type === "text/markdown" || type === "text/latex") {
    return <MarkdownContent content={value} renderHtml />;
  }
  return <pre className="notebook__text"><AnsiText text={value} /></pre>;
}

function Output({ output }: { output: NotebookOutput }) {
  switch (output.kind) {
    case "stream":
      return (
        <pre className={`notebook__text${output.name === "stderr" ? " notebook__text--stderr" : ""}`}>
          <AnsiText text={output.text} />
        </pre>
      );
    case "error":
      return (
        <pre className="notebook__text notebook__text--error">
          <AnsiText text={output.traceback || `${output.name}: ${output.value}`} />
        </pre>
      );
    default:
      return <OutputData data={output.data} />;
  }
}

function prompt(count: number | null): string {
  return `[${count ?? " "}]`;
}

/**
 * A Jupyter notebook, read the way Jupyter shows it: Markdown cells rendered,
 * code cells coloured with their execution counts, and each output in the
 * richest form it carries — a plot as a picture, a DataFrame as its HTML table.
 * Nothing runs; HTML outputs are drawn the same script-free way an HTML file is.
 */
export function NotebookView({
  content,
  pathBaseDir,
  resolveImageSrc,
  onDocumentClick
}: {
  content: string;
  pathBaseDir: string | null;
  resolveImageSrc: (source: string) => string | null;
  onDocumentClick: (event: MouseEvent<HTMLDivElement>) => void;
}) {
  const { t } = useI18n();
  const notebook = useMemo(() => {
    try {
      return { ok: true as const, value: parseNotebook(content) };
    } catch (error) {
      return { ok: false as const, message: error instanceof Error ? error.message : String(error) };
    }
  }, [content]);

  if (!notebook.ok) {
    return (
      <p className="files-pane__error" role="alert">
        {t("这个笔记本不是有效的 JSON：{reason}", "This notebook is not valid JSON: {reason}", { reason: notebook.message })}
      </p>
    );
  }

  const language = fenceLanguage(notebook.value.language) ?? "python";
  return (
    <div className="file-preview file-preview--notebook" onClickCapture={onDocumentClick}>
      {notebook.value.cells.length === 0 && <p className="files-pane__notice">{t("这个笔记本没有单元格。", "This notebook has no cells.")}</p>}
      {notebook.value.cells.map((cell, index) => (
        <NotebookCellView
          key={index}
          cell={cell}
          language={language}
          pathBaseDir={pathBaseDir}
          resolveImageSrc={resolveImageSrc}
        />
      ))}
    </div>
  );
}

function NotebookCellView({
  cell,
  language,
  pathBaseDir,
  resolveImageSrc
}: {
  cell: NotebookCell;
  language: string;
  pathBaseDir: string | null;
  resolveImageSrc: (source: string) => string | null;
}) {
  if (cell.kind === "markdown") {
    // `attachment:name` is a picture pasted into the cell and stored with it.
    const resolve = (source: string) => {
      if (source.startsWith("attachment:")) {
        const bundle = cell.attachments[source.slice("attachment:".length)];
        const type = bundle && Object.keys(bundle).find((candidate) => candidate.startsWith("image/"));
        return bundle && type ? `data:${type};base64,${String(bundle[type]).replace(/\s+/g, "")}` : null;
      }
      return resolveImageSrc(source);
    };
    return (
      <section className="notebook__cell notebook__cell--markdown">
        <div className="notebook__row">
          <div className="notebook__prompt" aria-hidden="true" />
          <div className="notebook__body">
            <MarkdownContent content={cell.source} renderHtml linkifyPaths documentLinks pathBaseDir={pathBaseDir} resolveImageSrc={resolve} />
          </div>
        </div>
      </section>
    );
  }
  if (cell.kind === "raw") {
    return (
      <section className="notebook__cell notebook__cell--raw">
        <div className="notebook__row">
          <div className="notebook__prompt" aria-hidden="true" />
          <pre className="notebook__body notebook__text">{cell.source}</pre>
        </div>
      </section>
    );
  }
  return (
    <section className="notebook__cell notebook__cell--code">
      <div className="notebook__row">
        <div className="notebook__prompt notebook__prompt--in">{`In ${prompt(cell.executionCount)}:`}</div>
        <div className="notebook__body">
          <NotebookCode source={cell.source} language={language} />
        </div>
      </div>
      {cell.outputs.map((output, index) => (
        <div className="notebook__row notebook__row--output" key={index}>
          <div className="notebook__prompt notebook__prompt--out">
            {output.kind === "data" && output.result ? `Out ${prompt(output.executionCount)}:` : ""}
          </div>
          <div className="notebook__body notebook__output">
            <Output output={output} />
          </div>
        </div>
      ))}
    </section>
  );
}

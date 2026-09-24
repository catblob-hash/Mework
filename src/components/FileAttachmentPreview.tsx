import { Code2, Eye, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useState, type MouseEvent } from "react";
import { useI18n } from "../i18n";
import { externalHttpUrl } from "../lib/externalLinks";
import { codeLanguage, fileViewerKind, hasSourceForm } from "../lib/fileViewers";
import { splitFrontMatter } from "../lib/frontMatter";
import { fileAttachmentData } from "../lib/runtime";
import type { FileAttachment } from "../types";
import { MarkdownCodeBlock, NumberedCode } from "./CodeBlock";
import { Dialog, IconButton } from "./Common";
import { CsvTable } from "./FilePreview/CsvTable";
import { dataUrlBytes, formatBytes } from "./FilePreview/format";
import { HtmlPreview, type HtmlPreviewResources } from "./FilePreview/HtmlPreview";
import { ImageViewer } from "./FilePreview/ImageViewer";
import { NotebookView } from "./FilePreview/NotebookView";
import { PdfViewer } from "./FilePreview/PdfViewer";
import { MarkdownContent } from "./MarkdownContent";
import "./FilePreview/FilePreview.css";
import "./FileAttachmentPreview.css";

const fileDataCache = new Map<string, Promise<string>>();
/** Files run larger than thumbnails; a handful of recent ones is plenty to reopen quickly. */
const FILE_DATA_CACHE_LIMIT = 6;

function fileData(file: FileAttachment): Promise<string> {
  const cached = fileDataCache.get(file.id);
  if (cached) {
    fileDataCache.delete(file.id);
    fileDataCache.set(file.id, cached);
    return cached;
  }
  const request = fileAttachmentData(file.id).catch((error) => {
    fileDataCache.delete(file.id);
    throw error;
  });
  fileDataCache.set(file.id, request);
  while (fileDataCache.size > FILE_DATA_CACHE_LIMIT) {
    const oldest = fileDataCache.keys().next().value as string | undefined;
    if (!oldest) break;
    fileDataCache.delete(oldest);
  }
  return request;
}

/** An attachment is not in the workspace, so a page in it reads nothing beside itself. */
const NO_RESOURCES: HtmlPreviewResources = {
  readText: async () => null,
  readImage: async () => null
};

function decodeText(source: string): string {
  const text = new TextDecoder("utf-8").decode(dataUrlBytes(source));
  return text.startsWith("\uFEFF") ? text.slice(1) : text;
}

/** Only pictures the file carries in itself; anything it would fetch stays out. */
function inlineImagesOnly(source: string): string | null {
  return source.startsWith("data:image/") ? source : null;
}

/**
 * Links inside an attachment: the open web goes to the system browser the way
 * every link in the app does, a fragment scrolls, and anything else — a path
 * into a workspace the file never lived in — goes nowhere rather than
 * navigating the window away from the app.
 */
function onDocumentClick(event: MouseEvent<HTMLDivElement>): void {
  if (event.defaultPrevented || event.button !== 0) return;
  const node = event.target instanceof Element ? event.target.closest("a[href]") : null;
  if (!(node instanceof HTMLAnchorElement)) return;
  const href = node.getAttribute("href") ?? "";
  if (externalHttpUrl(href) !== null) return;
  event.preventDefault();
  if (href.startsWith("#") && href.length > 1) {
    const container = event.currentTarget;
    const id = decodeURIComponent(href.slice(1));
    const target = container.querySelector(`#${CSS.escape(id)}`)
      ?? container.querySelector(`#${CSS.escape(`user-content-${id}`)}`);
    target?.scrollIntoView({ block: "start" });
  }
}

export function formatTokenEstimate(tokens: number): string {
  if (tokens < 1000) return String(tokens);
  const thousands = tokens / 1000;
  return `${thousands >= 100 ? Math.round(thousands) : thousands.toFixed(1)}k`;
}

type Loaded =
  | { status: "loading" }
  | { status: "ready"; source: string }
  | { status: "error"; message: string };

function PreviewBody({
  file,
  source,
  showSource
}: {
  file: FileAttachment;
  source: string;
  showSource: boolean;
}) {
  const { t } = useI18n();
  const text = useMemo(() => (file.format === "text" ? decodeText(source) : ""), [file.format, source]);
  if (file.format === "pdf") {
    return <PdfViewer key={file.id} source={source} bytes={file.bytes} />;
  }
  if (!text) {
    return <p className="attachment-preview__notice">{t("这个文件是空的。", "This file is empty.")}</p>;
  }
  const kind = fileViewerKind(file.name);
  if (!showSource) {
    switch (kind) {
      case "markdown": {
        const frontMatter = splitFrontMatter(text);
        return (
          <div className="attachment-preview__document" onClickCapture={onDocumentClick}>
            {frontMatter && (
              <div className="attachment-preview__front-matter markdown-content">
                <MarkdownCodeBlock code={frontMatter.source} language={frontMatter.language} label={frontMatter.language} />
              </div>
            )}
            <MarkdownContent
              content={frontMatter ? frontMatter.body : text}
              renderHtml
              documentLinks
              resolveImageSrc={inlineImagesOnly}
            />
          </div>
        );
      }
      case "html":
        return <HtmlPreview key={file.id} path={file.name} content={text} resources={NO_RESOURCES} onOpenFile={() => undefined} />;
      case "csv":
        return <CsvTable key={file.id} path={file.name} content={text} />;
      case "notebook":
        return (
          <div className="attachment-preview__document attachment-preview__document--flush">
            <NotebookView
              content={text}
              pathBaseDir={null}
              resolveImageSrc={inlineImagesOnly}
              onDocumentClick={onDocumentClick}
            />
          </div>
        );
      case "image":
        // A text file that is a picture: SVG.
        return (
          <ImageViewer
            key={file.id}
            source={`data:image/svg+xml;base64,${source.slice(source.indexOf(",") + 1)}`}
            name={file.name}
            bytes={file.bytes}
          />
        );
      default:
        break;
    }
  }
  return (
    <NumberedCode
      className="attachment-preview__code"
      content={text}
      language={codeLanguage(file.name)}
      label={file.name}
    />
  );
}

/**
 * One attached file, opened: a PDF laid out, a document rendered, a table
 * tabulated, and anything else as its own numbered, coloured text — the Files
 * pane's readers, fed from the attachment store instead of the workspace.
 */
export function FileAttachmentPreview({ file, onClose }: { file: FileAttachment; onClose: () => void }) {
  const { t } = useI18n();
  const [loaded, setLoaded] = useState<Loaded>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);
  const [showSource, setShowSource] = useState(false);
  const switchable = file.format === "text" && hasSourceForm(file.name);

  useEffect(() => {
    let active = true;
    setLoaded({ status: "loading" });
    // A retry asks the host again rather than replaying the failure it cached.
    if (attempt > 0) fileDataCache.delete(file.id);
    fileData(file).then(
      (source) => {
        if (active) setLoaded({ status: "ready", source });
      },
      (error: unknown) => {
        if (active) setLoaded({ status: "error", message: error instanceof Error ? error.message : String(error) });
      }
    );
    return () => {
      active = false;
    };
  }, [file, attempt]);

  const retry = useCallback(() => setAttempt((value) => value + 1), []);

  const details = [
    file.format === "pdf"
      ? t("PDF · {pages} 页", "PDF · {pages} pages", { pages: file.pages ?? 0 })
      : null,
    formatBytes(file.bytes),
    t("约 {tokens} tokens", "≈{tokens} tokens", { tokens: formatTokenEstimate(file.tokens) })
  ].filter(Boolean).join(" · ");

  return (
    <Dialog
      title={file.name}
      onClose={onClose}
      width="1080px"
      className="attachment-preview"
      bodyClassName="attachment-preview__body"
    >
      <div className="attachment-preview__bar">
        <span className="attachment-preview__details">
          {details}
          {file.format === "pdf" ? (
            <span className="attachment-preview__hint">
              {t("模型读取的是其中的文字", "The model reads its text")}
            </span>
          ) : null}
        </span>
        {switchable ? (
          <IconButton
            label={showSource ? t("显示渲染结果", "Show rendered") : t("显示源文本", "Show source")}
            className="attachment-preview__toggle"
            onClick={() => setShowSource((value) => !value)}
          >
            {showSource ? <Eye size={14} /> : <Code2 size={14} />}
          </IconButton>
        ) : null}
      </div>
      <div className="attachment-preview__viewer file-preview">
        {loaded.status === "loading" ? (
          <p className="attachment-preview__notice">{t("正在读取…", "Loading…")}</p>
        ) : loaded.status === "error" ? (
          <div className="attachment-preview__error" role="alert">
            <span>{t("无法读取这个附件：{reason}", "Could not read this attachment: {reason}", { reason: loaded.message })}</span>
            <button type="button" onClick={retry}>
              <RefreshCw size={13} aria-hidden="true" />
              {t("重试", "Retry")}
            </button>
          </div>
        ) : (
          <PreviewBody file={file} source={loaded.source} showSource={showSource} />
        )}
      </div>
    </Dialog>
  );
}

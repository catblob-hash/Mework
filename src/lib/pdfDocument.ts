import { dataUrlBytes } from "../components/FilePreview/format";

export type PdfJs = typeof import("pdfjs-dist/legacy/build/pdf.mjs");

/**
 * pdf.js, loaded on the first PDF and kept.
 *
 * The legacy build is the one that carries its own polyfills: the modern build
 * leans on built-ins (`Map.prototype.getOrInsertComputed`) that neither the macOS
 * WebView nor an older WebView2 is guaranteed to have. The worker's code runs on
 * the main thread — handed to pdf.js as `globalThis.pdfjsWorker` — because the
 * renderer's CSP allows no workers at all.
 */
let pdfJs: Promise<PdfJs> | null = null;

export function loadPdfJs(): Promise<PdfJs> {
  pdfJs ??= Promise.all([
    import("pdfjs-dist/legacy/build/pdf.mjs"),
    import("pdfjs-dist/legacy/build/pdf.worker.mjs")
  ]).then(([library, worker]) => {
    (globalThis as { pdfjsWorker?: unknown }).pdfjsWorker = worker;
    return library;
  });
  return pdfJs;
}

/**
 * The CMaps and standard fonts pdf.js asks for by name, bundled into the app.
 *
 * pdf.js would fetch them from a URL, and the CSP allows no fetches but the
 * host's own IPC. Without CMaps a PDF that relies on the predefined CJK encodings
 * shows no text; without the standard fonts, a PDF that names Helvetica without
 * embedding it falls back to whatever the system has. Each file is its own chunk,
 * read only when a document asks for it.
 */
const CMAPS = import.meta.glob<string>("/node_modules/pdfjs-dist/cmaps/*.bcmap", { query: "?inline", import: "default" });
const STANDARD_FONTS = import.meta.glob<string>("/node_modules/pdfjs-dist/standard_fonts/*.{pfb,ttf}", {
  query: "?inline",
  import: "default"
});

class BundledBinaryDataFactory {
  async fetch({ kind, filename }: { kind: string; filename: string }): Promise<Uint8Array> {
    const table = kind === "cMapUrl" ? CMAPS : kind === "standardFontDataUrl" ? STANDARD_FONTS : null;
    const directory = kind === "cMapUrl" ? "cmaps" : "standard_fonts";
    const load = table?.[`/node_modules/pdfjs-dist/${directory}/${filename}`];
    if (!load) throw new Error(`No bundled ${kind} data named ${filename}`);
    return dataUrlBytes(await load());
  }
}

/** Starts loading a PDF with the options every reader in the app needs. */
export function openPdfDocument(pdfjs: PdfJs, data: Uint8Array) {
  return pdfjs.getDocument({
    data,
    useWasm: false,
    useWorkerFetch: false,
    BinaryDataFactory: BundledBinaryDataFactory,
    // Only their presence matters: the factory above answers every name.
    cMapUrl: "bundled:/cmaps/",
    cMapPacked: true,
    standardFontDataUrl: "bundled:/standard_fonts/",
    enableXfa: false
  });
}

export interface ExtractedPdfText {
  text: string;
  pages: number;
}

/** Thrown for a PDF that opens but whose pages carry no text layer — a scan, usually. */
export class PdfWithoutTextError extends Error {}
/** Thrown for a PDF that asks for a password. */
export class PdfPasswordError extends Error {}

/**
 * The text a PDF's pages carry, page by page, for a model that reads text.
 *
 * Each page is headed `[Page N]` so the model can cite where something was
 * said. Lines follow pdf.js's own end-of-line marks; nothing is reflowed, since
 * any guess at columns or reading order would be wrong as often as right. A
 * page with nothing on it keeps its heading, so page numbers stay true.
 */
export async function extractPdfText(bytes: Uint8Array): Promise<ExtractedPdfText> {
  const pdfjs = await loadPdfJs();
  // pdf.js takes ownership of (and detaches) the buffer it is handed.
  const task = openPdfDocument(pdfjs, bytes.slice());
  try {
    let document: Awaited<typeof task.promise>;
    try {
      document = await task.promise;
    } catch (reason) {
      const name = typeof reason === "object" && reason !== null && "name" in reason
        ? String((reason as { name: unknown }).name)
        : "";
      if (name === "PasswordException") throw new PdfPasswordError(String(reason));
      throw reason;
    }
    const pages: string[] = [];
    let anyText = false;
    for (let index = 1; index <= document.numPages; index += 1) {
      const page = await document.getPage(index);
      const content = await page.getTextContent();
      let text = "";
      for (const item of content.items) {
        if (!("str" in item)) continue;
        text += item.str;
        if (item.hasEOL) text += "\n";
      }
      const trimmed = text.replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
      if (trimmed) anyText = true;
      pages.push(`[Page ${index}]\n${trimmed}`);
      page.cleanup();
    }
    if (!anyText) throw new PdfWithoutTextError("PDF has no text layer");
    return { text: pages.join("\n\n"), pages: document.numPages };
  } finally {
    void task.destroy();
  }
}

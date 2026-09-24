/// <reference types="vite/client" />

declare module "*.css";

/** pdf.js ships no declarations for its worker; the viewer only hands the module back to pdf.js. */
declare module "pdfjs-dist/legacy/build/pdf.worker.mjs" {
  export const WorkerMessageHandler: unknown;
}

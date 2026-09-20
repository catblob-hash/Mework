import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import App from "./App";
import { hasBackendRuntime } from "./lib/backend";
import {
  seedMemoryE2EDocument,
  validateMemoryE2ESeedConfig
} from "./lib/memoryE2ESeed";
import {
  flushDocumentSaves,
  loadDocument,
  saveDocument
} from "./lib/runtime";
import { startApplicationAppearance } from "./theme";
import "katex/dist/katex.min.css";
import "./styles.css";

const summary = document.querySelector<HTMLOutputElement>("#memory-e2e-summary")!;

function mountApp(): void {
  startApplicationAppearance();
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App />
    </StrictMode>
  );
}

async function main(): Promise<void> {
  if (import.meta.env.VITE_MEMORY_E2E_ENABLED !== "1" || !hasBackendRuntime()) {
    throw new Error("此页面只能由 npm run dev:memory-e2e 的隔离 browser-dev runner 启动");
  }
  const config = validateMemoryE2ESeedConfig({
    runId: import.meta.env.VITE_MEMORY_E2E_RUN_ID?.trim() ?? "",
    protocolBaseUrl: import.meta.env.VITE_MEMORY_E2E_PROTOCOL_BASE_URL?.trim() ?? ""
  });
  const current = await loadDocument();
  const seeded = seedMemoryE2EDocument(current, config);
  await saveDocument(seeded, { durable: true });
  await flushDocumentSaves();
  summary.dataset.state = "ready";
  summary.textContent = [
    "READY",
    "已在随机隔离数据目录种入 OpenAI Responses / Chat / Anthropic provider；",
    "三个 provider 共享精确模型 ID kimi-k3，Responses 另含大小写隔离探针 Kimi-K3；",
    "工作区仍须通过 UI 的无参 picker 获取宿主临时目录。"
  ].join(" ");
  mountApp();
}

void main().catch((error) => {
  const message = error instanceof Error ? error.message : String(error);
  summary.dataset.state = "failed";
  summary.textContent = `FAIL ${message}`;
  document.getElementById("root")!.textContent = message;
});

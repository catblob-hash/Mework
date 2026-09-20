// Resident memory: idle versus after a 1000-delta flood. Use tasklist for the
// actual working set rather than process.memoryUsage(), which measures sidecar-reported memory.
import { spawn, execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { once } from "node:events";

const bin = process.argv[2] ?? "dist/mework-aisdk.exe";
const rss = (pid) => {
  try {
    const out = execFileSync("tasklist", ["/FI", `PID eq ${pid}`, "/FO", "CSV", "/NH"], { encoding: "utf8" });
    // The last quoted field is memory, for example `"45,678 K"`; commas are
    // part of the number, so extract quoted fields rather than splitting on commas.
    const fields = out.match(/"([^"]*)"/g);
    const last = fields?.[fields.length - 1] ?? "";
    const kb = Number(last.replace(/[^0-9]/g, ""));
    return Number.isFinite(kb) ? kb / 1024 : 0;
  } catch { return 0; }
};

const server = createServer(async (req, res) => {
  for await (const _ of req) {}
  res.writeHead(200, { "content-type": "text/event-stream" });
  const c = (d, f = null, u) => JSON.stringify({ id: "x", object: "chat.completion.chunk", created: 1, model: "m", choices: [{ index: 0, delta: d, finish_reason: f }], ...(u ? { usage: u } : {}) });
  res.write(`data: ${c({ role: "assistant", content: "" })}\n\n`);
  for (let i = 0; i < 1000; i += 1) res.write(`data: ${c({ content: `${i};` })}\n\n`);
  res.write(`data: ${c({}, "stop", { prompt_tokens: 1, completion_tokens: 1000, total_tokens: 1001 })}\n\n`);
  res.write("data: [DONE]\n\n");
  res.end();
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
const baseURL = `http://127.0.0.1:${server.address().port}/v1`;

const t0 = Date.now();
const child = spawn(bin, [], { stdio: ["pipe", "pipe", "ignore"] });
let buf = "";
const seen = [];
child.stdout.on("data", (d) => {
  buf += d.toString();
  for (;;) { const i = buf.indexOf("\n"); if (i < 0) break; const l = buf.slice(0, i); buf = buf.slice(i + 1); if (l) seen.push(JSON.parse(l)); }
});
const waitFor = (m) => new Promise((r) => { const t = setInterval(() => { const f = seen.find(m); if (f) { clearInterval(t); r(f); } }, 5); });

child.stdin.write(JSON.stringify({ v: 1, type: "hello" }) + "\n");
await waitFor((f) => f.type === "ready");
const ready = Date.now() - t0;
await new Promise((r) => setTimeout(r, 300));
const idle = rss(child.pid);

child.stdin.write(JSON.stringify({ v: 1, type: "step", id: "f", payload: { family: "openai-compatible", baseURL, apiKey: "sk-x", modelId: "flood", messages: [{ role: "user", content: "hi" }], maxSteps: 1 } }) + "\n");
await waitFor((f) => f.type === "done" && f.id === "f");
const after = rss(child.pid);

console.log(`冷启动(hello→ready) ${ready} ms`);
console.log(`空闲 RSS            ${idle.toFixed(1)} MiB`);
console.log(`1000 delta 之后 RSS  ${after.toFixed(1)} MiB`);
child.kill();
server.close();

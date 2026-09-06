"use strict";

import fs from "node:fs";
import readline from "node:readline";

const input = readline.createInterface({
  input: process.stdin,
  crlfDelay: Infinity,
});

let callCount = 0;
let listCount = 0;
let pingAcknowledged = process.env.MCP_MOCK_IDLE_PING !== "1";
const idlePingId = "mework-idle-ping";
// A request for this method is never answered, so the caller can observe what
// happens while a request is in flight. Writing the marker file first turns the
// wait into a barrier instead of a guess about timing.
const hangMethod = process.env.MCP_MOCK_HANG_METHOD || "";
const hangMarkerPath = process.env.MCP_MOCK_MARKER || "";
const declarePrompts = process.env.MCP_MOCK_DECLARE_PROMPTS === "1";

function send(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

input.on("line", (line) => {
  if (!line.trim()) {
    return;
  }
  const message = JSON.parse(line);
  if (
    message.id === idlePingId &&
    message.result &&
    typeof message.result === "object"
  ) {
    pingAcknowledged = true;
    return;
  }
  if (hangMethod && message.method === hangMethod) {
    if (hangMarkerPath) {
      fs.writeFileSync(hangMarkerPath, "1");
    }
    return;
  }
  if (message.method === "initialize") {
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        protocolVersion: "2025-11-25",
        capabilities: declarePrompts
          ? { tools: { listChanged: false }, prompts: { listChanged: false } }
          : { tools: { listChanged: false } },
        serverInfo: { name: "mework-mock", version: "1.0.0" },
      },
    });
    return;
  }
  if (message.method === "notifications/initialized") {
    return;
  }
  if (message.method === "tools/list") {
    if (process.env.MCP_MOCK_EMPTY_FIRST_LIST === "1" && listCount++ === 0) {
      send({ jsonrpc: "2.0", id: message.id, result: { tools: [] } });
      return;
    }
    if (!message.params || !message.params.cursor) {
      send({
        jsonrpc: "2.0",
        id: message.id,
        result: {
          tools: [
            {
              name: "echo",
              title: "Echo",
              description: "Echo a value",
              inputSchema: {
                type: "object",
                properties: { value: { type: "string" } },
                required: ["value"],
              },
            },
          ],
          nextCursor: "second-page",
        },
      });
    } else {
      send({
        jsonrpc: "2.0",
        id: message.id,
        result: {
          tools: [
            {
              name: "add",
              title: "Add",
              description: "Add two numbers",
              inputSchema: {
                type: "object",
                properties: {
                  left: { type: "number" },
                  right: { type: "number" },
                },
                required: ["left", "right"],
              },
            },
          ],
        },
      });
      if (!pingAcknowledged) {
        send({
          jsonrpc: "2.0",
          id: idlePingId,
          method: "ping",
          params: {},
        });
      }
    }
    return;
  }
  if (message.method === "tools/call") {
    if (!pingAcknowledged) {
      send({
        jsonrpc: "2.0",
        id: message.id,
        error: { code: -32001, message: "Idle ping was not acknowledged" },
      });
      return;
    }
    const args = message.params.arguments || {};
    callCount += 1;
    if (message.params.name === "echo") {
      const respond = () =>
        send({
          jsonrpc: "2.0",
          id: message.id,
          result: {
            content: [{ type: "text", text: String(args.value ?? "") }],
            structuredContent: {
              echo: String(args.value ?? ""),
              callCount,
              cwd: process.cwd(),
            },
            isError: false,
          },
        });
      const delayMs = Number(args.delayMs || 0);
      if (Number.isFinite(delayMs) && delayMs > 0) {
        setTimeout(respond, Math.min(delayMs, 5000));
      } else {
        respond();
      }
      return;
    }
    if (message.params.name === "add") {
      const sum = Number(args.left || 0) + Number(args.right || 0);
      send({
        jsonrpc: "2.0",
        id: message.id,
        result: {
          content: [{ type: "text", text: String(sum) }],
          structuredContent: { sum },
          isError: false,
        },
      });
      return;
    }
  }
  if (Object.prototype.hasOwnProperty.call(message, "id")) {
    send({
      jsonrpc: "2.0",
      id: message.id,
      error: { code: -32601, message: "Method not found" },
    });
  }
});

import process from "node:process";

const browserDevClients = new Map([
  ["--codex", { client: "codex", label: "Codex" }],
  ["--claude", { client: "claude", label: "Claude Code" }]
]);

const exactTauriDevNpmCommand =
  /^[^\S\r\n]*npm(?:\.cmd)?\s+run\s+(?:(["'])tauri:dev\1|tauri:dev)[^\S\r\n]*$/i;
const tauriDevNpmCommandSegment =
  /(^[^\S\r\n]*|(?:&&|\|\||[;&|])[^\S\r\n]*|\r?\n[^\S\r\n]*|\([^\S\r\n]*)(?:npm(?:\.cmd)?\s+run\s+(?:(["'])tauri:dev\2|tauri:dev))(?=$|[\s;&|)])/im;

function parseClient(args) {
  if (args.length !== 1 || !browserDevClients.has(args[0])) {
    throw new Error(
      "browser-dev command hook 必须且只能指定 --codex 或 --claude"
    );
  }
  return browserDevClients.get(args[0]);
}

async function main() {
  const descriptor = parseClient(process.argv.slice(2));
  let rawInput = "";
  process.stdin.setEncoding("utf8");
  for await (const chunk of process.stdin) rawInput += chunk;

  let input;
  try {
    input = JSON.parse(rawInput);
  } catch {
    process.stderr.write("Could not parse the PreToolUse hook input.\n");
    process.exitCode = 2;
    return;
  }

  const toolInput = input?.tool_input;
  const command = toolInput?.command;
  const supportedTool =
    input?.tool_name === "Bash" || input?.tool_name === "shell_command";
  if (
    input?.hook_event_name !== "PreToolUse"
    || !supportedTool
    || typeof command !== "string"
  ) {
    return;
  }

  if (!exactTauriDevNpmCommand.test(command)) {
    if (!tauriDevNpmCommandSegment.test(command)) return;
    process.stdout.write(
      JSON.stringify({
        hookSpecificOutput: {
          hookEventName: "PreToolUse",
          permissionDecision: "deny",
          permissionDecisionReason:
            `请单独运行 npm run dev:browser -- --${descriptor.client}；`
            + "带参数或链式 tauri:dev 不会被自动批准。"
        }
      })
    );
    return;
  }

  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "allow",
        updatedInput: {
          ...toolInput,
          command: `npm run dev:browser -- --${descriptor.client}`
        },
        additionalContext:
          `The workspace hook routed native Tauri debugging through the shared ${descriptor.label} browser bridge. Continue in the ${descriptor.label} built-in browser.`
      }
    })
  );
}

main().catch((error) => {
  process.stderr.write(`${error.stack ?? error.message}\n`);
  process.exitCode = 2;
});

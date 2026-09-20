#!/usr/bin/env node
// Formal verification pipeline (docs/formal-methods.md). ProB connects the
// authoritative CSP interaction protocol, TLA+ safety model, and Rust
// implementation projection. Every stage is mandatory:
//
//   1. Vocabulary guard: compare TLA+ actions, CSP channel declarations, and
//      Rust `KernelEvent::NAMES` byte-for-byte. Parsing is fail-closed because
//      csp_guide silently accepts unknown channel names as plain CSP events.
//   2. Model checking: require probcli to report all states visited with no
//      counterexample; timeout, crash, and partial exploration fail.
//   3. TLA mutation checks: every model guard needs a guard-removal mutant that
//      fails model checking with a counterexample.
//   4. CSP protocol: accepted and rejected scenario refinements, deadlock
//      obligations, CSP-TLA composition, arity negative control, and CSP
//      mutations with specified assertion inversions.
//   5. Trace agreement: export kernel traces; TLA must replay accepted traces
//      perfectly and CSP MAIN must refine them. Rejected probes require both a
//      valid prefix and rejection at their final event.
//
// Usage:
//   node scripts/formal-verify.mjs               # all stages (default scope)
//   node scripts/formal-verify.mjs --wide        # wider model-checking scope
//   node scripts/formal-verify.mjs --no-mutants  # skip TLA and CSP mutations
//   node scripts/formal-verify.mjs --no-traces   # skip cargo tests and replay
//   node scripts/formal-verify.mjs --no-csp      # skip CSP stages

import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { ensureProb, probDir, runProbcli } from "./prob-fetch.mjs";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..");
const tlaPath = path.join(repoRoot, "formal", "tla", "AgentKernel.tla");
const cfgPath = path.join(repoRoot, "formal", "tla", "AgentKernel.cfg");
const cspPath = path.join(repoRoot, "formal", "csp", "AgentKernel.csp");
const traceDir = path.join(repoRoot, "target-formal-traces");

function log(message) {
  process.stdout.write(`[formal-verify] ${message}\n`);
}

function fail(message) {
  process.stderr.write(`[formal-verify] 失败：${message}\n`);
  process.exit(1);
}

function tail(text, n = 1600) {
  return (text ?? "").slice(-n);
}

// Prefer an explicit JAVA_PATH to the portable JRE, independently of PATH.
function javaPathArgs() {
  const manifest = JSON.parse(readFileSync(path.join(probDir, "PROB-MANIFEST.json"), "utf8"));
  const java = path.join(probDir, "jre", manifest.jre.release, "bin", "java.exe");
  if (!existsSync(java)) fail(`便携 JRE 缺失：${java}（重跑 npm run prob:fetch）`);
  return ["-p", "JAVA_PATH", java.replaceAll("\\", "/")];
}

// ---------------------------------------------------------------------------
// Classify probcli results. Timeout, signal, missing exit status, and crashes
// must not impersonate a verification result.
// ---------------------------------------------------------------------------

function classify(result) {
  if (result.error) return { kind: "tool-error", detail: result.error.message };
  if (result.signal) return { kind: "tool-error", detail: `被信号 ${result.signal} 终止` };
  if (typeof result.status !== "number") return { kind: "tool-error", detail: "无退出码（超时？）" };
  const out = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  const allVisited = /No counter example found\. ALL states visited/.test(out);
  const counterexample = /COUNTER EXAMPLE FOUND|invariant_violation/.test(out);
  const perfectReplay = /Perfect replay possible/.test(out);
  const replayError = /replay_json_trace_file/.test(out) && /error occurred/.test(out);
  return { kind: "ran", status: result.status, allVisited, counterexample, perfectReplay, replayError, out };
}

function expectModelCheckPass(result, label) {
  const c = classify(result);
  if (c.kind === "tool-error") fail(`${label}：probcli 未正常完成（${c.detail}）`);
  if (c.status !== 0 || !c.allVisited) {
    fail(`${label}：模型检查未通过或未穷尽状态空间：\n${tail(c.out, 3000)}`);
  }
  return c;
}

function expectCounterexample(result, label) {
  const c = classify(result);
  if (c.kind === "tool-error") fail(`${label}：probcli 未正常完成（${c.detail}）——不算「被抓住」`);
  if (c.status === 0 || !c.counterexample) {
    fail(`${label}：期望模型检查给出不变量反例，实际：status=${c.status}\n${tail(c.out, 2000)}`);
  }
}

function expectPerfectReplay(result, label) {
  const c = classify(result);
  if (c.kind === "tool-error") fail(`${label}：probcli 未正常完成（${c.detail}）`);
  if (c.status !== 0 || !c.perfectReplay) {
    fail(`${label}：未被模型完美回放（内核接受了模型不允许的行为）：\n${tail(c.out, 3000)}`);
  }
}

function expectReplayRefused(result, label) {
  const c = classify(result);
  if (c.kind === "tool-error") fail(`${label}：probcli 未正常完成（${c.detail}）——不算「被拒绝」`);
  if (c.status === 0 || c.perfectReplay || !c.replayError) {
    fail(`${label}：期望回放在末事件被拒绝，实际：status=${c.status}\n${tail(c.out, 2000)}`);
  }
}

// ---------------------------------------------------------------------------
// Classify CSP assertion results. Exit status alone cannot distinguish syntax
// or evaluation errors from rejected refinement and detected deadlocks.
// ---------------------------------------------------------------------------

function classifyCsp(result) {
  if (result.error) return { kind: "tool-error", detail: result.error.message };
  if (result.signal) return { kind: "tool-error", detail: `被信号 ${result.signal} 终止` };
  if (typeof result.status !== "number") return { kind: "tool-error", detail: "无退出码（超时？）" };
  const out = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  return {
    kind: "ran",
    status: result.status,
    allOk: /==> Model Check Successful/.test(out),
    refinementFails: /refinement_check_fails/.test(out),
    deadlockFound: /reaches a deadlock/.test(out),
    out,
  };
}

// Individual refinement and mutation assertions normally finish quickly; this
// is a ceiling against hangs. Deadlock obligations exhaust the whole CSP state
// space and use the dedicated CSP_DEADLOCK_TIMEOUT below.
function cspAssert(assertions, file, timeout = 1_200_000) {
  const args = ["-strict"];
  for (const assertion of assertions) args.push("-csp_assertion", assertion);
  args.push(file);
  return runProbcli(args, { timeout });
}

function expectCspPass(result, label) {
  const c = classifyCsp(result);
  if (c.kind === "tool-error") fail(`${label}：probcli 未正常完成（${c.detail}）`);
  if (c.status !== 0 || !c.allOk) {
    fail(`${label}：CSP 断言未全部通过：status=${c.status}\n${tail(c.out, 3000)}`);
  }
}

function expectCspRefinementRefused(result, label) {
  const c = classifyCsp(result);
  if (c.kind === "tool-error") fail(`${label}：probcli 未正常完成（${c.detail}）——不算「被拒绝」`);
  if (c.status === 0 || c.allOk || !c.refinementFails) {
    fail(`${label}：期望迹精化被拒（refinement_check_fails），实际：status=${c.status}\n${tail(c.out, 2000)}`);
  }
}

function expectCspDeadlockFound(result, label) {
  const c = classifyCsp(result);
  if (c.kind === "tool-error") fail(`${label}：probcli 未正常完成（${c.detail}）——不算「发现死锁」`);
  if (c.status === 0 || c.allOk || !c.deadlockFound) {
    fail(`${label}：期望发现死锁，实际：status=${c.status}\n${tail(c.out, 2000)}`);
  }
}

// ---------------------------------------------------------------------------
// Stage 1: fail-closed vocabulary guard
// ---------------------------------------------------------------------------

function tlaActionNames(tlaText) {
  const nextBlock = tlaText.match(/\nNext ==\n([\s\S]*?)\n\nSpec ==/);
  if (!nextBlock) fail("在 AgentKernel.tla 里找不到 Next == …… Spec == 结构");
  const names = [];
  for (const raw of nextBlock[1].split("\n")) {
    const line = raw.trim();
    if (line === "") continue;
    if (!line.startsWith("\\/")) fail(`Next 里出现无法解析的行（每个分支必须单行、以 \\/ 开头）：${raw}`);
    const body = line.slice(2).trim();
    const match = body.match(/^(?:\\E [^:]+ : )?([A-Za-z_]\w*)(?:\([\w\s,]*\))?$/);
    if (!match) fail(`无法从 Next 分支解析出唯一动作名：${raw}`);
    names.push(match[1]);
  }
  if (names.length === 0) fail("Next 里没有解析出任何动作");
  if (new Set(names).size !== names.length) fail(`Next 分支存在重复动作：${names.join(", ")}`);
  return names;
}

// Parse CSP channel declarations fail-closed. This guard prevents renamed
// channels from silently passing through csp_guide as plain CSP events; arity
// errors fail hard, but name alignment requires byte-for-byte comparison.
function cspChannelNames(cspText) {
  const names = [];
  for (const raw of cspText.split("\n")) {
    const line = raw.trim();
    if (!line.startsWith("channel ")) continue;
    const decl = line.slice("channel ".length).split(":")[0];
    for (const piece of decl.split(",")) {
      const name = piece.trim();
      if (!/^[A-Za-z_]\w*$/.test(name)) fail(`无法解析 CSP 通道声明：${raw}`);
      names.push(name);
    }
  }
  if (names.length === 0) fail("AgentKernel.csp 里没有解析出任何通道声明");
  if (new Set(names).size !== names.length) fail(`CSP 通道声明存在重复：${names.join(", ")}`);
  return names;
}

// Static vocabulary guard: TLA action names versus CSP channel names without
// requiring traces exported by cargo tests.
function checkVocabulary() {
  const fromTla = tlaActionNames(readFileSync(tlaPath, "utf8"));
  const fromCsp = cspChannelNames(readFileSync(cspPath, "utf8"));
  const tlaSet = [...fromTla].sort().join(",");
  const cspSet = [...fromCsp].sort().join(",");
  if (tlaSet !== cspSet) {
    fail(`事件词汇不一致：\n  TLA+ ${fromTla.join(", ")}\n  CSP  ${fromCsp.join(", ")}`);
  }
  log(`词汇守卫通过：TLA+ 与 CSP 的 ${fromTla.length} 个事件名逐字一致`);
  return fromTla;
}

function checkAlphabet() {
  const fromTla = tlaActionNames(readFileSync(tlaPath, "utf8"));
  const alphabetFile = path.join(traceDir, "alphabet.json");
  if (!existsSync(alphabetFile)) {
    fail(`缺少 ${alphabetFile}（应由 agent-kernel 的一致性测试导出）`);
  }
  const fromRust = JSON.parse(readFileSync(alphabetFile, "utf8")).events;
  const tlaSet = [...fromTla].sort().join(",");
  const rustSet = [...fromRust].sort().join(",");
  if (tlaSet !== rustSet) {
    fail(`事件词汇不一致：\n  TLA+  ${fromTla.join(", ")}\n  Rust  ${fromRust.join(", ")}`);
  }
  log(`词汇守卫通过：TLA+ 与 Rust 的 ${fromTla.length} 个事件名逐字一致`);
}

// ---------------------------------------------------------------------------
// Stage 2: model checking and optional wider scope
// ---------------------------------------------------------------------------

function modelCheck(tla) {
  return runProbcli([tla, ...javaPathArgs(), "-model_check", "-strict"], { timeout: 900_000 });
}

function checkModel() {
  const c = expectModelCheckPass(modelCheck(tlaPath), "基线模型");
  const states = c.out.match(/States analysed: (\d+)/)?.[1] ?? "?";
  log(`模型检查通过：${states} 个状态全访问，无反例`);
}

function checkModelWide() {
  const text = readFileSync(tlaPath, "utf8");
  const widened = text
    .replace("\nCalls == 1..2\n", "\nCalls == 1..3\n")
    .replace("\nAgents == 1..2\n", "\nAgents == 1..3\n")
    .replace("\nMaxLiveAgents == 1\n", "\nMaxLiveAgents == 2\n")
    .replace("\nTaskIds == 1..2\n", "\nTaskIds == 1..3\n");
  if (widened === text) fail("--wide：scope 定义替换未生效（模型的 scope 行变了？）");
  const work = mkdtempSync(path.join(os.tmpdir(), "formal-wide-"));
  try {
    const wideTla = path.join(work, "AgentKernel.tla");
    writeFileSync(wideTla, widened);
    writeFileSync(path.join(work, "AgentKernel.cfg"), readFileSync(cfgPath));
    log("宽 scope 模型检查（Calls/Agents=1..3, MaxLive=2，约 8 分钟）……");
    const c = expectModelCheckPass(modelCheck(wideTla), "宽 scope 模型");
    const states = c.out.match(/States analysed: (\d+)/)?.[1] ?? "?";
    log(`宽 scope 通过：${states} 个状态全访问，无反例`);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

// ---------------------------------------------------------------------------
// Stage 3: guard-removal mutation matrix
// ---------------------------------------------------------------------------
// Every guard needs a removal mutant. Needles are anchored by action name and
// must occur exactly once; `expect` documents the intended invariant while the
// actual criterion is a model-checking counterexample.

const MUTANTS = [
  { name: "S11-唤醒首轮遗漏已完成任务", expect: "InvTurnAudit",
    needle: '  /\\ wakeFoldPending = {}\n  /\\ round = "prep"\n',
    replacement: '  /\\ round = "prep"\n' },
  { name: "TurnStart 无视 idle 前提", expect: "InvTurnAudit",
    needle: 'TurnStart ==\n  /\\ turn = "idle"\n',
    replacement: "TurnStart ==\n  /\\ TRUE\n" },
  { name: "TurnCancel 可从任意相位触发", expect: "InvTurnAudit",
    needle: 'TurnCancel ==\n  /\\ turn = "running"\n',
    replacement: "TurnCancel ==\n  /\\ TRUE\n" },
  { name: "S8-取消不作废未决等待", expect: "InvWaitPending",
    needle: '  /\\ turn\' = "cancelling"\n  /\\ agentWaited\' = [a \\in Agents |-> FALSE]\n',
    replacement: '  /\\ turn\' = "cancelling"\n  /\\ agentWaited\' = agentWaited\n' },
  { name: "TurnEnd 可从 idle 触发", expect: "InvTurnAudit",
    needle: 'TurnEnd ==\n  /\\ turn \\in {"running", "cancelling"}\n',
    replacement: "TurnEnd ==\n  /\\ TRUE\n" },
  { name: "S5S9-轮内收束回合", expect: "InvTurnAudit/InvQuiescentIdle",
    needle: '  /\\ round = "prep"\n  /\\ \\A a \\in Agents : \\/ agentPhase[a] \\in {"none", "running"}\n',
    replacement: '  /\\ \\A a \\in Agents : \\/ agentPhase[a] \\in {"none", "running"}\n' },
  { name: "S11-正常收束也带走 done", expect: "InvTurnAudit",
    needle: '                       \\/ (turn = "cancelling" /\\ agentPhase[a] = "done")\n',
    replacement: '                       \\/ agentPhase[a] = "done"\n' },
  { name: "S9-义务未清或带未决 steer 正常收束", expect: "InvTurnAudit",
    needle: '  /\\ (turn = "cancelling" \\/ (~roundDue /\\ ~steerPending))\n  /\\ turn\' = "idle"\n',
    replacement: '  /\\ turn\' = "idle"\n' },
  { name: "S8-运行中仍可编辑上下文", expect: "InvTurnAudit",
    needle: 'ContextEdit ==\n  /\\ turn = "idle"\n',
    replacement: "ContextEdit ==\n  /\\ TRUE\n" },
  { name: "S4S9-非 running 回合开轮", expect: "InvTurnAudit",
    needle: 'RoundStart ==\n  /\\ turn = "running"\n',
    replacement: "RoundStart ==\n  /\\ TRUE\n" },
  { name: "S9-轮内再开轮", expect: "InvTurnAudit",
    needle: '  /\\ round = "prep"\n  /\\ roundDue\n',
    replacement: '  /\\ roundDue\n' },
  { name: "S9-空转轮", expect: "InvTurnAudit",
    needle: '  /\\ roundDue\n  /\\ ~steerPending\n',
    replacement: '  /\\ ~steerPending\n' },
  { name: "S9-带未决 steer 开轮", expect: "InvTurnAudit",
    needle: '  /\\ ~steerPending\n  /\\ round\' = "inround"\n',
    replacement: '  /\\ round\' = "inround"\n' },
  { name: "S9-重复关轮", expect: "InvTurnAudit",
    needle: 'RoundEnd ==\n  /\\ round = "inround"\n',
    replacement: "RoundEnd ==\n  /\\ TRUE\n" },
  { name: "S5-轮带未定局调用收束", expect: "InvTurnAudit/InvCallsInRound",
    needle: '  /\\ \\A c \\in Calls : callPhase[c] = "unused"\n  /\\ \\A a \\in Agents : ~agentWaited[a]\n',
    replacement: '  /\\ \\A a \\in Agents : ~agentWaited[a]\n' },
  { name: "S8-轮带未决等待收束", expect: "InvTurnAudit/InvWaitPending",
    needle: '  /\\ \\A a \\in Agents : ~agentWaited[a]\n  /\\ round\' = "prep"\n',
    replacement: '  /\\ round\' = "prep"\n' },
  { name: "S9-idle 可入 steer", expect: "InvTurnAudit",
    needle: 'SteerEnqueue ==\n  /\\ turn \\in {"running", "cancelling"}\n',
    replacement: "SteerEnqueue ==\n  /\\ TRUE\n" },
  { name: "S9-非 running 汇入 steer", expect: "InvTurnAudit",
    needle: 'SteerJoin ==\n  /\\ turn = "running"\n',
    replacement: "SteerJoin ==\n  /\\ TRUE\n" },
  { name: "S9-轮内汇入 steer", expect: "InvTurnAudit",
    needle: '  /\\ round = "prep"\n  /\\ steerPending\n',
    replacement: '  /\\ steerPending\n' },
  { name: "S9-无 steer 也可汇入", expect: "InvTurnAudit",
    needle: '  /\\ steerPending\n  /\\ steerPending\' = FALSE\n',
    replacement: '  /\\ steerPending\' = FALSE\n' },
  { name: "S4-取消或空闲期间仍受理新请求", expect: "InvCallAudit",
    needle: 'ToolRequest(c, d) ==\n  /\\ turn = "running"\n',
    replacement: "ToolRequest(c, d) ==\n  /\\ TRUE\n" },
  { name: "S9-prep 窗口受理新请求", expect: "InvCallAudit",
    needle: '  /\\ round = "inround"\n  /\\ callPhase[c] = "unused"\n',
    replacement: '  /\\ callPhase[c] = "unused"\n' },
  { name: "ToolRequest 可覆盖未定局槽位", expect: "InvCallAudit",
    needle: '  /\\ callPhase[c] = "unused"\n  /\\ callPhase\' = [callPhase EXCEPT ![c] = "requested"]\n',
    replacement: '  /\\ callPhase\' = [callPhase EXCEPT ![c] = "requested"]\n' },
  { name: "S1-危险调用被自动放行", expect: "InvGrantMatchesDanger",
    needle: '  /\\ ~callDanger[c]\n  /\\ callPhase\' = [callPhase EXCEPT ![c] = "allowed"]\n  /\\ callFresh\' = [callFresh EXCEPT ![c] = TRUE]\n  /\\ callGrant\' = [callGrant EXCEPT ![c] = "auto"]\n',
    replacement: '  /\\ TRUE\n  /\\ callPhase\' = [callPhase EXCEPT ![c] = "allowed"]\n  /\\ callFresh\' = [callFresh EXCEPT ![c] = TRUE]\n  /\\ callGrant\' = [callGrant EXCEPT ![c] = "auto"]\n' },
  { name: "S4-取消期间仍可放行", expect: "InvCallAudit",
    needle: 'ToolAllow(c) ==\n  /\\ turn = "running"\n',
    replacement: "ToolAllow(c) ==\n  /\\ TRUE\n" },
  { name: "ToolAllow 可作用于任意相位", expect: "InvCallAudit",
    needle: '  /\\ callPhase[c] \\in {"requested", "allowed"}\n  /\\ ~callDanger[c]\n',
    replacement: '  /\\ ~callDanger[c]\n' },
  { name: "S4-取消期间仍可批准", expect: "InvCallAudit",
    needle: 'ToolApprove(c) ==\n  /\\ turn = "running"\n',
    replacement: "ToolApprove(c) ==\n  /\\ TRUE\n" },
  { name: "ToolApprove 可作用于任意相位", expect: "InvCallAudit",
    needle: '  /\\ callPhase[c] \\in {"requested", "allowed"}\n  /\\ callDanger[c]\n',
    replacement: '  /\\ callDanger[c]\n' },
  { name: "S1-非危险调用也走用户批准", expect: "InvGrantMatchesDanger",
    needle: '  /\\ callDanger[c]\n  /\\ callPhase\' = [callPhase EXCEPT ![c] = "allowed"]\n',
    replacement: '  /\\ TRUE\n  /\\ callPhase\' = [callPhase EXCEPT ![c] = "allowed"]\n' },
  { name: "S4-取消期间仍可拒绝", expect: "InvCallAudit",
    needle: 'ToolDeny(c) ==\n  /\\ turn = "running"\n',
    replacement: "ToolDeny(c) ==\n  /\\ TRUE\n" },
  { name: "ToolDeny 可作用于任意相位", expect: "InvCallAudit",
    needle: '  /\\ callPhase[c] = "requested"\n  /\\ callPhase\' = [callPhase EXCEPT ![c] = "denied"]\n',
    replacement: '  /\\ TRUE\n  /\\ callPhase\' = [callPhase EXCEPT ![c] = "denied"]\n' },
  { name: "S4-取消期间仍可开始执行", expect: "InvCallAudit",
    needle: 'ToolExecStart(c) ==\n  /\\ turn = "running"\n',
    replacement: "ToolExecStart(c) ==\n  /\\ TRUE\n" },
  { name: "S1S2S3-执行不再消费新鲜批准", expect: "InvCallAudit",
    needle: '  /\\ callFresh[c]\n  /\\ callPhase\' = [callPhase EXCEPT ![c] = "executing"]\n',
    replacement: '  /\\ TRUE\n  /\\ callPhase\' = [callPhase EXCEPT ![c] = "executing"]\n' },
  { name: "S7-任意相位都能出回执", expect: "InvCallAudit",
    needle: 'ToolExecEnd(c) ==\n  /\\ callPhase[c] \\in {"executing", "execbg"}\n',
    replacement: "ToolExecEnd(c) ==\n  /\\ TRUE\n" },
  { name: "S12-取消期间仍可交接后台", expect: "InvCallAudit",
    needle: 'ToolBackground(c, a, r) ==\n  /\\ turn = "running"\n',
    replacement: "ToolBackground(c, a, r) ==\n  /\\ TRUE\n" },
  { name: "S12-非执行相位也能交接后台", expect: "InvCallAudit",
    needle: '  /\\ callPhase[c] = "executing"\n  /\\ Cardinality(RunningAgents) < MaxLiveAgents\n',
    replacement: "  /\\ Cardinality(RunningAgents) < MaxLiveAgents\n" },
  { name: "S12-交接后台无视并发上限", expect: "InvCapacity",
    needle: '  /\\ Cardinality(RunningAgents) < MaxLiveAgents\n  /\\ agentPhase[a] = "none"\n',
    replacement: '  /\\ agentPhase[a] = "none"\n' },
  { name: "S12-交接后台可覆盖占用中的任务槽", expect: "InvAgentAudit",
    needle: '  /\\ agentPhase[a] = "none"\n  /\\ r # taskRid[a]\n',
    replacement: "  /\\ r # taskRid[a]\n" },
  { name: "S12-交接后台不轮替身份", expect: "InvAgentAudit",
    needle: '  /\\ r # taskRid[a]\n  /\\ callPhase\' = [callPhase EXCEPT ![c] = "execbg"]\n',
    replacement: '  /\\ callPhase\' = [callPhase EXCEPT ![c] = "execbg"]\n' },
  { name: "S5-运行中也可丢弃未定局调用", expect: "InvCallAudit",
    needle: '  /\\ \\/ callPhase[c] \\in {"done", "denied"}\n     \\/ /\\ callPhase[c] \\in {"requested", "allowed"}\n        /\\ turn = "cancelling"\n',
    replacement: "  /\\ TRUE\n" },
  { name: "S4-取消期间仍可派生子代理", expect: "InvAgentAudit",
    needle: 'AgentSpawn(a, r) ==\n  /\\ turn = "running"\n',
    replacement: "AgentSpawn(a, r) ==\n  /\\ TRUE\n" },
  { name: "S9-prep 窗口派生", expect: "InvAgentAudit",
    needle: '  /\\ round = "inround"\n  /\\ agentPhase[a] = "none"\n',
    replacement: '  /\\ agentPhase[a] = "none"\n' },
  { name: "AgentSpawn 可覆盖在途任务槽位", expect: "InvAgentAudit",
    needle: '  /\\ agentPhase[a] = "none"\n  /\\ Cardinality(RunningAgents) < MaxLiveAgents\n',
    replacement: '  /\\ Cardinality(RunningAgents) < MaxLiveAgents\n' },
  { name: "S6-子代理并发上限失效", expect: "InvAgentAudit/InvCapacity",
    needle: '  /\\ Cardinality(RunningAgents) < MaxLiveAgents\n  /\\ r # taskRid[a]\n',
    replacement: '  /\\ r # taskRid[a]\n' },
  { name: "S10-重复身份的新派生", expect: "InvAgentAudit",
    needle: '  /\\ r # taskRid[a]\n  /\\ agentPhase\' = [agentPhase EXCEPT ![a] = "running"]\n',
    replacement: '  /\\ agentPhase\' = [agentPhase EXCEPT ![a] = "running"]\n' },
  { name: "AgentComplete 可作用于任意相位", expect: "InvAgentAudit",
    needle: 'AgentComplete(a, r) ==\n  /\\ agentPhase[a] = "running"\n',
    replacement: "AgentComplete(a, r) ==\n  /\\ TRUE\n" },
  { name: "S10-完成不核身份", expect: "InvAgentAudit",
    needle: '  /\\ r = taskRid[a]\n  /\\ agentPhase\' = [agentPhase EXCEPT ![a] = "done"]\n',
    replacement: '  /\\ agentPhase\' = [agentPhase EXCEPT ![a] = "done"]\n' },
  { name: "S9-取消收束期间也可 fold", expect: "InvAgentAudit",
    needle: 'AgentFold(a, r) ==\n  /\\ turn = "running"\n',
    replacement: "AgentFold(a, r) ==\n  /\\ TRUE\n" },
  { name: "S9-轮内 fold", expect: "InvAgentAudit",
    needle: '  /\\ round = "prep"\n  /\\ agentPhase[a] = "done"\n',
    replacement: '  /\\ agentPhase[a] = "done"\n' },
  { name: "S9-运行中的任务也被 fold", expect: "InvAgentAudit",
    needle: '  /\\ round = "prep"\n  /\\ agentPhase[a] = "done"\n  /\\ r = taskRid[a]\n',
    replacement: '  /\\ round = "prep"\n  /\\ agentPhase[a] \\in {"running", "done"}\n  /\\ r = taskRid[a]\n' },
  { name: "S10-fold 不核身份", expect: "InvAgentAudit",
    needle: '  /\\ r = taskRid[a]\n  /\\ agentPhase\' = [agentPhase EXCEPT ![a] = "none"]\n  /\\ roundDue\' = TRUE\n',
    replacement: '  /\\ agentPhase\' = [agentPhase EXCEPT ![a] = "none"]\n  /\\ roundDue\' = TRUE\n' },
  { name: "S8S4-取消或空闲期间仍可发起等待", expect: "InvAgentAudit",
    needle: 'TaskWait(a, r) ==\n  /\\ turn = "running"\n',
    replacement: "TaskWait(a, r) ==\n  /\\ TRUE\n" },
  { name: "S9-prep 窗口发起等待", expect: "InvAgentAudit",
    needle: '  /\\ round = "inround"\n  /\\ agentPhase[a] # "none"\n',
    replacement: '  /\\ agentPhase[a] # "none"\n' },
  { name: "S8-不存在的任务也能被等待", expect: "InvAgentAudit",
    needle: '  /\\ agentPhase[a] # "none"\n  /\\ ~agentWaited[a]\n',
    replacement: '  /\\ ~agentWaited[a]\n' },
  { name: "S8-同一任务重复发起等待", expect: "InvAgentAudit",
    needle: '  /\\ ~agentWaited[a]\n  /\\ r = taskRid[a]\n',
    replacement: '  /\\ r = taskRid[a]\n' },
  { name: "S10-发起等待不核身份", expect: "InvAgentAudit",
    needle: '  /\\ r = taskRid[a]\n  /\\ agentWaited\' = [agentWaited EXCEPT ![a] = TRUE]\n',
    replacement: '  /\\ agentWaited\' = [agentWaited EXCEPT ![a] = TRUE]\n' },
  { name: "S8-未发起等待也能超时撤回", expect: "InvAgentAudit",
    needle: 'TaskWaitTimeout(a, r) ==\n  /\\ agentWaited[a]\n',
    replacement: "TaskWaitTimeout(a, r) ==\n  /\\ TRUE\n" },
  { name: "S8-已settle的等待也可超时撤回", expect: "InvAgentAudit",
    needle: '  /\\ agentWaited[a]\n  /\\ agentPhase[a] = "running"\n',
    replacement: '  /\\ agentWaited[a]\n' },
  { name: "S10-超时撤回不核身份", expect: "InvAgentAudit",
    needle: '  /\\ r = taskRid[a]\n  /\\ roundDue\' = TRUE\n',
    replacement: '  /\\ roundDue\' = TRUE\n' },
  { name: "S8-未发起等待也能交付", expect: "InvAgentAudit",
    needle: 'TaskWaitDeliver(a, r) ==\n  /\\ agentWaited[a]\n',
    replacement: "TaskWaitDeliver(a, r) ==\n  /\\ TRUE\n" },
  { name: "S8-未settle的任务也能被交付", expect: "InvAgentAudit",
    needle: '  /\\ agentPhase[a] = "done"\n  /\\ r = taskRid[a]\n  /\\ agentPhase\' = [agentPhase EXCEPT ![a] = "none"]\n  /\\ agentWaited\' = [agentWaited EXCEPT ![a] = FALSE]\n',
    replacement: '  /\\ r = taskRid[a]\n  /\\ agentPhase\' = [agentPhase EXCEPT ![a] = "none"]\n  /\\ agentWaited\' = [agentWaited EXCEPT ![a] = FALSE]\n' },
  { name: "S10-交付不核身份", expect: "InvAgentAudit",
    needle: '  /\\ r = taskRid[a]\n  /\\ agentPhase\' = [agentPhase EXCEPT ![a] = "none"]\n  /\\ agentWaited\' = [agentWaited EXCEPT ![a] = FALSE]\n',
    replacement: '  /\\ agentPhase\' = [agentPhase EXCEPT ![a] = "none"]\n  /\\ agentWaited\' = [agentWaited EXCEPT ![a] = FALSE]\n' },
  { name: "S8-交付不清等待位", expect: "InvWaitPending",
    needle: '  /\\ agentWaited\' = [agentWaited EXCEPT ![a] = FALSE]\n  /\\ roundDue\' = TRUE\n',
    replacement: '  /\\ agentWaited\' = agentWaited\n  /\\ roundDue\' = TRUE\n' },
];

function occurrences(haystack, needle) {
  let count = 0;
  let index = haystack.indexOf(needle);
  while (index !== -1) {
    count += 1;
    index = haystack.indexOf(needle, index + 1);
  }
  return count;
}

function checkMutants() {
  const tlaText = readFileSync(tlaPath, "utf8");
  const work = mkdtempSync(path.join(os.tmpdir(), "formal-mutants-"));
  try {
    for (const mutant of MUTANTS) {
      const found = occurrences(tlaText, mutant.needle);
      if (found !== 1) {
        fail(`变异「${mutant.name}」的定位串出现 ${found} 次（应恰好 1 次）——模型正文变了，先更新 MUTANTS`);
      }
      const mutantTla = path.join(work, "AgentKernel.tla");
      writeFileSync(mutantTla, tlaText.replace(mutant.needle, mutant.replacement));
      writeFileSync(path.join(work, "AgentKernel.cfg"), readFileSync(cfgPath));
      expectCounterexample(modelCheck(mutantTla), `变异「${mutant.name}」（预期 ${mutant.expect}）`);
      log(`变异自检通过：「${mutant.name}」被抓住`);
    }
    log(`变异矩阵通过：${MUTANTS.length} 个守卫删除变异全部被反例抓住`);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

// ---------------------------------------------------------------------------
// Stage 4: CSP protocol layer
// ---------------------------------------------------------------------------
// Accepted scenarios must refine MAIN; rejected scenarios must fail with
// refinement_check_fails rather than a parse error. Scenario names are checked
// against AgentKernel.csp. MAIN and NoAdversary must be deadlock-free; the
// latter excludes PolicyTighten and SteerEnqueue to represent turn convergence.

const CSP_SCENARIOS_OK = [
  "SCEN_WAKE_TWO_FOLDS",
  "SCEN_WAKE_CANCEL_RETRY",
  "SCEN_WAKE_LATE_COMPLETE",
  "SCEN_APPROVE",
  "SCEN_REAPPROVE",
  "SCEN_CANCEL",
  "SCEN_ASYNC_WAIT",
  "SCEN_WAIT_SETTLED",
  "SCEN_WAIT_TIMEOUT",
  "SCEN_FOLD",
  "SCEN_STEER",
  "SCEN_STEER_CARRYOVER",
  "SCEN_STOP_RETRIGGERS",
  "SCEN_TID_ROTATE",
  "SCEN_TASK_SURVIVES_END",
  "SCEN_ADOPT",
  "SCEN_CANCEL_RUNNING_SURVIVES",
  "SCEN_CANCEL_DONE_CARRYOVER",
  "SCEN_CANCEL_WINDOW_COMPLETE",
  "SCEN_BG_TIMEOUT",
  "SCEN_BG_WAIT",
];

const CSP_SCENARIOS_BAD = [
  "BAD_WAKE_FIRST_ROUND",
  "BAD_WAKE_PARTIAL_FOLD",
  "BAD_WAKE_CANCEL_RETRY",
  "BAD_WAKE_STALE_FOLD",
  "BAD_EXEC_UNAPPROVED",
  "BAD_ALLOW_DANGER",
  "BAD_APPROVE_SAFE",
  "BAD_STALE_EXEC",
  "BAD_STALE_EXEC_SAFE",
  "BAD_DOUBLE_EXEC",
  "BAD_REQUEST_AFTER_CANCEL",
  "BAD_SPAWN_AFTER_CANCEL",
  "BAD_WAIT_AFTER_CANCEL",
  "BAD_WAIT_AFTER_CANCEL_RUNNING",
  "BAD_FOLD_AFTER_CANCEL",
  "BAD_ROUNDSTART_AFTER_CANCEL",
  "BAD_END_PENDING_DONE",
  "BAD_END_INROUND",
  "BAD_ROUNDEND_UNSETTLED_CALL",
  "BAD_DROP_PENDING_RUNNING",
  "BAD_EDIT_RUNNING",
  "BAD_EDIT_CANCELLING",
  "BAD_OVERCAP",
  "BAD_RECEIPT_NO_EXEC",
  "BAD_END_NO_ROUND",
  "BAD_IDLE_ROUND",
  "BAD_END_AFTER_FOLD",
  "BAD_END_AFTER_JOIN",
  "BAD_END_UNDELIVERED",
  "BAD_STEER_IDLE",
  "BAD_STEER_JOIN_INROUND",
  "BAD_STEER_JOIN_NO_PENDING",
  "BAD_ROUNDSTART_PENDING_STEER",
  "BAD_END_PENDING_STEER",
  "BAD_FOLD_RUNNING",
  "BAD_FOLD_INROUND",
  "BAD_WAIT_IN_PREP",
  "BAD_REQUEST_IN_PREP",
  "BAD_SPAWN_IN_PREP",
  "BAD_WAIT_NO_TASK",
  "BAD_DOUBLE_WAIT",
  "BAD_DELIVER_UNSETTLED",
  "BAD_DELIVER_NO_WAIT",
  "BAD_ROUNDEND_PENDING_WAIT",
  "BAD_ROUNDEND_PENDING_WAIT_SETTLED",
  "BAD_DELIVER_AFTER_CANCEL",
  "BAD_TIMEOUT_NO_WAIT",
  "BAD_TIMEOUT_SETTLED",
  "BAD_END_AFTER_TIMEOUT",
  "BAD_REQUEST_OVERWRITE",
  "BAD_ALLOW_EXECUTING",
  "BAD_APPROVE_EXECUTING",
  "BAD_DENY_ALLOWED",
  "BAD_SPAWN_OVERWRITE_DONE",
  "BAD_DOUBLE_COMPLETE",
  "BAD_TID_SPAWN_REPEAT",
  "BAD_TID_COMPLETE",
  "BAD_TID_WAIT",
  "BAD_TID_TIMEOUT",
  "BAD_TID_WAIT_DONE",
  "BAD_TID_COMPLETE_WAITING",
  "BAD_TID_DELIVER",
  "BAD_TID_FOLD",
  "BAD_TID_COMPLETE_CANCEL",
  "BAD_FOLD_IDLE",
  "BAD_TID_COMPLETE_IDLE",
  "BAD_BG_BEFORE_EXEC",
  "BAD_BG_AFTER_END",
  "BAD_BG_TWICE",
  "BAD_BG_STALE_ID",
  "BAD_BG_SLOT_DONE",
  "BAD_BG_OVERCAP",
];

const CSP_DEADLOCK_OBLIGATIONS = [
  "MAIN :[deadlock free [F]]",
  "NoAdversary :[deadlock free [F]]",
];

// Deadlock obligations enumerate the complete CSP state space and need their
// own timeout budget, independent from the fast scenario assertions.
const CSP_DEADLOCK_TIMEOUT = 5_400_000;

function checkCspScenarios() {
  const cspText = readFileSync(cspPath, "utf8");
  for (const name of [...CSP_SCENARIOS_OK, ...CSP_SCENARIOS_BAD]) {
    if (!cspText.includes(`${name} =`)) {
      fail(`场景进程 ${name} 不在 AgentKernel.csp 里——电池名单与协议正文漂移`);
    }
  }
  expectCspPass(
    cspAssert(CSP_SCENARIOS_OK.map((s) => `MAIN [T= ${s}`), cspPath),
    "CSP 正例场景",
  );
  log(`CSP 正例通过：${CSP_SCENARIOS_OK.length} 个场景被协议接受`);
  expectCspPass(
    cspAssert(CSP_DEADLOCK_OBLIGATIONS, cspPath, CSP_DEADLOCK_TIMEOUT),
    "CSP 死锁义务",
  );
  log("CSP 死锁义务通过：MAIN/NoAdversary 无死锁");
  for (const name of CSP_SCENARIOS_BAD) {
    expectCspRefinementRefused(cspAssert([`MAIN [T= ${name}`], cspPath), `CSP 反例 ${name}`);
  }
  log(`CSP 反例通过：${CSP_SCENARIOS_BAD.length} 个违例场景全部被迹精化拒绝`);
}

// CSP-TLA composition model-checks the safety machine under protocol guidance.
// It verifies channel-name and parameter mappings, no invariant counterexample,
// and termination. Unknown CSP channel names require the vocabulary guard;
// combined state counts are not comparable with either standalone state count.

function cspGuideCheck(csp) {
  // Composition enumerates a larger state space, so it has a dedicated budget.
  // Timeout remains a tool error rather than a verification result.
  return runProbcli(
    [tlaPath, ...javaPathArgs(), "-csp_guide", csp, "-model_check", "-nodead", "-strict"],
    { timeout: 7_200_000 },
  );
}

function checkCspComposition() {
  const c = expectModelCheckPass(cspGuideCheck(cspPath), "CSP‖TLA 组合");
  const states = c.out.match(/States analysed: (\d+)/)?.[1] ?? "?";
  log(`CSP‖TLA 组合通过：协议引导下 ${states} 个联合状态全访问，无不变量反例`);

  const text = readFileSync(cspPath, "utf8");
  const needle =
    "channel ToolAllow, ToolApprove, ToolDeny, ToolExecStart, ToolExecEnd, ToolSettle : CallSlots";
  if (!text.includes(needle)) fail("组合错例控制：通道声明行变了，先更新控制变异");
  const broken = text
    .replace(
      needle,
      "channel ToolApprove, ToolDeny, ToolExecStart, ToolExecEnd, ToolSettle : CallSlots\nchannel ToolAllow : CallSlots.Bool",
    )
    .replace(/ToolAllow\.([A-Za-z0-9]+)/g, "ToolAllow.$1.false");
  const work = mkdtempSync(path.join(os.tmpdir(), "formal-csp-ctl-"));
  try {
    const brokenPath = path.join(work, "AgentKernelBadArity.csp");
    writeFileSync(brokenPath, broken);
    const r = classifyCsp(cspGuideCheck(brokenPath));
    if (r.kind === "tool-error") fail(`组合错例控制：probcli 未正常完成（${r.detail}）`);
    if (r.status === 0 || !/CSP Channel has too many parameters/.test(r.out)) {
      fail(`组合错例控制：期望元数断裂被类型检查抓住，实际 status=${r.status}\n${tail(r.out, 1500)}`);
    }
    log("组合错例控制通过：通道元数断裂被响亮拒绝");
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

// CSP mutation matrix: each protocol constraint needs a mutant whose specified
// assertion flips. `pass` accepts a bad scenario after relaxation;
// `refine-fail` rejects a good scenario after tightening; `deadlock` makes
// NoAdversary deadlock. Unique needle counts keep the matrix aligned with CSP.

const MUTANTS_CSP = [
  { name: "S11-唤醒首轮跳过 fold", assertion: "MAIN [T= BAD_WAKE_FIRST_ROUND", expectOnMutant: "pass",
    needle: "AIdleDone(a, r) = TurnStart -> AWakeDone(a, r)\n",
    replacement: "AIdleDone(a, r) = TurnStart -> ADone(a, r)\n" },
  { name: "S11-只交付部分继承结果就开轮", assertion: "MAIN [T= BAD_WAKE_PARTIAL_FOLD", expectOnMutant: "pass",
    needle: "AWakeDone(a, r) = AgentFold.a.r -> ANone(a, r)\n",
    replacement: "AWakeDone(a, r) = RoundStart -> AWakeDone(a, r)\n               [] AgentFold.a.r -> ANone(a, r)\n" },
  { name: "S11-取消后丢失首轮交付义务", assertion: "MAIN [T= BAD_WAKE_CANCEL_RETRY", expectOnMutant: "pass",
    needle: "AIdleDone(a, r) = TurnStart -> AWakeDone(a, r)\n",
    replacement: "AIdleDone(a, r) = TurnStart -> ADone(a, r)\n" },
  { name: "S11-任务槽不参与开轮同步", assertion: "MAIN [T= BAD_WAKE_FIRST_ROUND", expectOnMutant: "pass",
    needle: "AgentAlpha(a) = union(TurnEvents,\n                      union(union({RoundStart, RoundEnd},\n",
    replacement: "AgentAlpha(a) = union(TurnEvents,\n                      union(union({RoundEnd},\n" },
  { name: "S11-继承结果 fold 不核身份", assertion: "MAIN [T= BAD_WAKE_STALE_FOLD", expectOnMutant: "pass",
    needle: "AWakeDone(a, r) = AgentFold.a.r -> ANone(a, r)\n",
    replacement: "AWakeDone(a, r) = AgentFold.a?rr -> ANone(a, r)\n" },
  { name: "S11-继承结果不可 fold", assertion: "MAIN [T= SCEN_WAKE_TWO_FOLDS", expectOnMutant: "refine-fail",
    needle: "AWakeDone(a, r) = AgentFold.a.r -> ANone(a, r)\n",
    replacement: "AWakeDone(a, r) = STOP\n" },
  { name: "S11-继承结果不可取消保留", assertion: "MAIN [T= SCEN_WAKE_CANCEL_RETRY", expectOnMutant: "refine-fail",
    needle: "AWakeDone(a, r) = AgentFold.a.r -> ANone(a, r)\n               [] TurnCancel -> ADoneCx(a, r)\n",
    replacement: "AWakeDone(a, r) = AgentFold.a.r -> ANone(a, r)\n" },
  { name: "S3-收紧后旧批准仍可执行", assertion: "MAIN [T= BAD_STALE_EXEC", expectOnMutant: "pass",
    needle: "CAllowed(c, true, fresh) = fresh & ToolExecStart.c -> CExecuting(c)\n",
    replacement: "CAllowed(c, true, fresh) = ToolExecStart.c -> CExecuting(c)\n" },
  { name: "S3-收紧后旧放行仍可执行(非危险分支)", assertion: "MAIN [T= BAD_STALE_EXEC_SAFE", expectOnMutant: "pass",
    needle: "CAllowed(c, false, fresh) = fresh & ToolExecStart.c -> CExecuting(c)\n",
    replacement: "CAllowed(c, false, fresh) = ToolExecStart.c -> CExecuting(c)\n" },
  { name: "S1-危险调用被自动放行", assertion: "MAIN [T= BAD_ALLOW_DANGER", expectOnMutant: "pass",
    needle: "CRequested(c, true) = ToolApprove.c -> CAllowed(c, true, true)\n",
    replacement: "CRequested(c, true) = ToolAllow.c -> CAllowed(c, true, true)\n                   [] ToolApprove.c -> CAllowed(c, true, true)\n" },
  { name: "S1-非危险调用也走用户批准", assertion: "MAIN [T= BAD_APPROVE_SAFE", expectOnMutant: "pass",
    needle: "CRequested(c, false) = ToolAllow.c -> CAllowed(c, false, true)\n",
    replacement: "CRequested(c, false) = ToolApprove.c -> CAllowed(c, false, true)\n                    [] ToolAllow.c -> CAllowed(c, false, true)\n" },
  { name: "S1-未批准即可执行", assertion: "MAIN [T= BAD_EXEC_UNAPPROVED", expectOnMutant: "pass",
    needle: "CRequested(c, true) = ToolApprove.c -> CAllowed(c, true, true)\n",
    replacement: "CRequested(c, true) = ToolExecStart.c -> CExecuting(c)\n                   [] ToolApprove.c -> CAllowed(c, true, true)\n" },
  { name: "S2-回执后同一批准再次执行", assertion: "MAIN [T= BAD_DOUBLE_EXEC", expectOnMutant: "pass",
    needle: "CDone(c) = ToolSettle.c -> CUnused(c)\n",
    replacement: "CDone(c) = ToolExecStart.c -> CExecuting(c)\n        [] ToolSettle.c -> CUnused(c)\n" },
  { name: "S7-未执行也能出回执", assertion: "MAIN [T= BAD_RECEIPT_NO_EXEC", expectOnMutant: "pass",
    needle: "                         [] PolicyTighten -> CAllowed(c, false, false)\n",
    replacement: "                         [] ToolExecEnd.c -> CDone(c)\n                         [] PolicyTighten -> CAllowed(c, false, false)\n" },
  { name: "S5-运行中丢弃未定局调用", assertion: "MAIN [T= BAD_DROP_PENDING_RUNNING", expectOnMutant: "pass",
    needle: "CRequested(c, false) = ToolAllow.c -> CAllowed(c, false, true)\n",
    replacement: "CRequested(c, false) = ToolSettle.c -> CUnused(c)\n                    [] ToolAllow.c -> CAllowed(c, false, true)\n" },
  { name: "S4-取消后仍受理新请求", assertion: "MAIN [T= BAD_REQUEST_AFTER_CANCEL", expectOnMutant: "pass",
    needle: "CUnusedCx(c) = RoundEnd -> CUnusedCx(c)\n",
    replacement: "CUnusedCx(c) = ToolRequest.c?d -> CPendingCx(c)\n            [] RoundEnd -> CUnusedCx(c)\n" },
  { name: "S5-轮带未定局调用收束", assertion: "MAIN [T= BAD_ROUNDEND_UNSETTLED_CALL", expectOnMutant: "pass",
    needle: "CRequested(c, false) = ToolAllow.c -> CAllowed(c, false, true)\n",
    replacement: "CRequested(c, false) = RoundEnd -> CRequested(c, false)\n                    [] ToolAllow.c -> CAllowed(c, false, true)\n" },
  { name: "S4-取消后仍可派生任务", assertion: "MAIN [T= BAD_SPAWN_AFTER_CANCEL", expectOnMutant: "pass",
    needle: "ANoneCx(a, r) = RoundEnd -> ANoneCx(a, r)\n",
    replacement: "ANoneCx(a, r) = AgentSpawn.a!other(r) -> ARunningCx(a, other(r))\n             [] RoundEnd -> ANoneCx(a, r)\n" },
  { name: "S8S4-取消后仍可发起等待", assertion: "MAIN [T= BAD_WAIT_AFTER_CANCEL", expectOnMutant: "pass",
    needle: "ADoneCx(a, r) = RoundEnd -> ADoneCx(a, r)\n",
    replacement: "ADoneCx(a, r) = TaskWait.a.r -> ADoneCx(a, r)\n            [] RoundEnd -> ADoneCx(a, r)\n" },
  { name: "S8S4-取消后running任务仍可被发起等待", assertion: "MAIN [T= BAD_WAIT_AFTER_CANCEL_RUNNING", expectOnMutant: "pass",
    needle: "ARunningCx(a, r) = AgentComplete.a.r -> ADoneCx(a, r)\n",
    replacement: "ARunningCx(a, r) = TaskWait.a.r -> ARunningCx(a, r)\n                [] AgentComplete.a.r -> ADoneCx(a, r)\n" },
  { name: "S9S4-取消后仍可 fold", assertion: "MAIN [T= BAD_FOLD_AFTER_CANCEL", expectOnMutant: "pass",
    needle: "ADoneCx(a, r) = RoundEnd -> ADoneCx(a, r)\n",
    replacement: "ADoneCx(a, r) = AgentFold.a.r -> ANoneCx(a, r)\n            [] RoundEnd -> ADoneCx(a, r)\n" },
  { name: "S5-未定局槽位受理新请求", assertion: "MAIN [T= BAD_REQUEST_OVERWRITE", expectOnMutant: "pass",
    needle: "CRequested(c, false) = ToolAllow.c -> CAllowed(c, false, true)\n",
    replacement: "CRequested(c, false) = ToolRequest.c?d -> CRequested(c, d)\n                    [] ToolAllow.c -> CAllowed(c, false, true)\n" },
  { name: "S1-执行中仍可重放行", assertion: "MAIN [T= BAD_ALLOW_EXECUTING", expectOnMutant: "pass",
    needle: "CExecuting(c) = ToolExecEnd.c -> CDone(c)\n",
    replacement: "CExecuting(c) = ToolAllow.c -> CExecuting(c)\n             [] ToolExecEnd.c -> CDone(c)\n" },
  { name: "S1-执行中仍可重批", assertion: "MAIN [T= BAD_APPROVE_EXECUTING", expectOnMutant: "pass",
    needle: "CExecuting(c) = ToolExecEnd.c -> CDone(c)\n",
    replacement: "CExecuting(c) = ToolApprove.c -> CExecuting(c)\n             [] ToolExecEnd.c -> CDone(c)\n" },
  { name: "S1-已放行调用仍可被拒绝", assertion: "MAIN [T= BAD_DENY_ALLOWED", expectOnMutant: "pass",
    needle: "CAllowed(c, false, fresh) = fresh & ToolExecStart.c -> CExecuting(c)\n",
    replacement: "CAllowed(c, false, fresh) = ToolDeny.c -> CDenied(c)\n                         [] fresh & ToolExecStart.c -> CExecuting(c)\n" },
  { name: "S9-已settle未取走的槽位可被新派生覆盖", assertion: "MAIN [T= BAD_SPAWN_OVERWRITE_DONE", expectOnMutant: "pass",
    needle: "ADone(a, r) = TaskWait.a.r -> AWaitDone(a, r)\n",
    replacement: "ADone(a, r) = AgentSpawn.a!other(r) -> ARunning(a, other(r))\n           [] TaskWait.a.r -> AWaitDone(a, r)\n" },
  { name: "S10-已settle任务可再次完成", assertion: "MAIN [T= BAD_DOUBLE_COMPLETE", expectOnMutant: "pass",
    needle: "ADone(a, r) = TaskWait.a.r -> AWaitDone(a, r)\n",
    replacement: "ADone(a, r) = AgentComplete.a.r -> ADone(a, r)\n           [] TaskWait.a.r -> AWaitDone(a, r)\n" },
  { name: "S11-收窄:running 任务不再跨收束存活", assertion: "MAIN [T= SCEN_TASK_SURVIVES_END", expectOnMutant: "refine-fail",
    needle: "              [] TurnCancel -> ARunningCx(a, r)\n              [] TurnEnd -> AIdleRunning(a, r)\n",
    replacement: "              [] TurnCancel -> ARunningCx(a, r)\n" },
  { name: "S11-收窄:取消收束仍中断任务", assertion: "MAIN [T= SCEN_CANCEL_RUNNING_SURVIVES", expectOnMutant: "refine-fail",
    needle: "                [] RoundEnd -> ARunningCx(a, r)\n                [] TurnEnd -> AIdleRunning(a, r)\n",
    replacement: "                [] RoundEnd -> ARunningCx(a, r)\n" },
  { name: "S6-并发上限失效", assertion: "MAIN [T= BAD_OVERCAP", expectOnMutant: "pass",
    needle: "CAPACITY(n) = (n < MaxLiveAgents) & AgentSpawn?a?r -> CAPACITY(n + 1)\n",
    replacement: "CAPACITY(n) = AgentSpawn?a?r -> CAPACITY(n + 1)\n" },
  { name: "S8-运行中仍可编辑上下文", assertion: "MAIN [T= BAD_EDIT_RUNNING", expectOnMutant: "pass",
    needle: "TRUN(s) = (not s) & RoundStart -> TRUN(s)\n",
    replacement: "TRUN(s) = ContextEdit -> TRUN(s)\n       [] (not s) & RoundStart -> TRUN(s)\n" },
  { name: "S8-取消收束期间仍可编辑上下文", assertion: "MAIN [T= BAD_EDIT_CANCELLING", expectOnMutant: "pass",
    needle: "TCX(s) = SteerEnqueue -> TCX(true)\n",
    replacement: "TCX(s) = ContextEdit -> TCX(s)\n      [] SteerEnqueue -> TCX(true)\n" },
  { name: "S9-轮内可收束回合", assertion: "MAIN [T= BAD_END_INROUND", expectOnMutant: "pass",
    needle: "WRound(due, cx) = ToolRequest?c?d -> WRound(due, cx)\n",
    replacement: "WRound(due, cx) = TurnEnd -> WPrep(false, false)\n             [] ToolRequest?c?d -> WRound(due, cx)\n" },
  { name: "S9-空转轮", assertion: "MAIN [T= BAD_IDLE_ROUND", expectOnMutant: "pass",
    needle: "            [] due & RoundStart -> WRound(false, cx)\n",
    replacement: "            [] RoundStart -> WRound(false, cx)\n" },
  { name: "S9-用户消息不充值义务", assertion: "MAIN [T= BAD_END_NO_ROUND", expectOnMutant: "pass",
    needle: "WPrep(due, cx) = TurnStart -> WPrep(true, false)\n",
    replacement: "WPrep(due, cx) = TurnStart -> WPrep(due, false)\n" },
  { name: "S9-fold 不充值义务", assertion: "MAIN [T= BAD_END_AFTER_FOLD", expectOnMutant: "pass",
    needle: "            [] AgentFold?a?r -> WPrep(true, cx)\n",
    replacement: "            [] AgentFold?a?r -> WPrep(due, cx)\n" },
  { name: "S9-steer 汇入不充值义务", assertion: "MAIN [T= BAD_END_AFTER_JOIN", expectOnMutant: "pass",
    needle: "            [] SteerJoin -> WPrep(true, cx)\n",
    replacement: "            [] SteerJoin -> WPrep(due, cx)\n" },
  { name: "S9-回执交付不充值义务", assertion: "MAIN [T= BAD_END_UNDELIVERED", expectOnMutant: "pass",
    needle: "             [] ToolSettle?c -> WRound(true, cx)\n",
    replacement: "             [] ToolSettle?c -> WRound(due, cx)\n" },
  { name: "S9-轮内汇入 steer", assertion: "MAIN [T= BAD_STEER_JOIN_INROUND", expectOnMutant: "pass",
    needle: "             [] RoundEnd -> WPrep(due, cx)\n",
    replacement: "             [] SteerJoin -> WRound(true, cx)\n             [] RoundEnd -> WPrep(due, cx)\n" },
  { name: "S9-带未决 steer 开轮", assertion: "MAIN [T= BAD_ROUNDSTART_PENDING_STEER", expectOnMutant: "pass",
    needle: "TRUN(s) = (not s) & RoundStart -> TRUN(s)\n",
    replacement: "TRUN(s) = RoundStart -> TRUN(s)\n" },
  { name: "S9-带未决 steer 正常收束", assertion: "MAIN [T= BAD_END_PENDING_STEER", expectOnMutant: "pass",
    needle: "       [] (not s) & TurnEnd -> TURN(s)\n",
    replacement: "       [] TurnEnd -> TURN(s)\n" },
  { name: "S9-无 steer 也可汇入", assertion: "MAIN [T= BAD_STEER_JOIN_NO_PENDING", expectOnMutant: "pass",
    needle: "       [] s & SteerJoin -> TRUN(false)\n",
    replacement: "       [] SteerJoin -> TRUN(false)\n" },
  { name: "S9-idle 可入 steer", assertion: "MAIN [T= BAD_STEER_IDLE", expectOnMutant: "pass",
    needle: "TURN(s) = ContextEdit -> TURN(s)\n",
    replacement: "TURN(s) = SteerEnqueue -> TURN(true)\n       [] ContextEdit -> TURN(s)\n" },
  { name: "S9-运行中的任务也可 fold", assertion: "MAIN [T= BAD_FOLD_RUNNING", expectOnMutant: "pass",
    needle: "ARunning(a, r) = AgentComplete.a.r -> ADone(a, r)\n",
    replacement: "ARunning(a, r) = AgentFold.a.r -> ANone(a, r)\n              [] AgentComplete.a.r -> ADone(a, r)\n" },
  { name: "S9-轮内 fold", assertion: "MAIN [T= BAD_FOLD_INROUND", expectOnMutant: "pass",
    needle: "             [] TaskWait?a?r -> WRound(due, cx)\n",
    replacement: "             [] TaskWait?a?r -> WRound(due, cx)\n             [] AgentFold?a?r -> WRound(true, cx)\n" },
  { name: "S9-prep 窗口等待", assertion: "MAIN [T= BAD_WAIT_IN_PREP", expectOnMutant: "pass",
    needle: "            [] AgentFold?a?r -> WPrep(true, cx)\n",
    replacement: "            [] AgentFold?a?r -> WPrep(true, cx)\n            [] TaskWait?a?r -> WPrep(due, cx)\n" },
  { name: "S9-prep 窗口受理请求", assertion: "MAIN [T= BAD_REQUEST_IN_PREP", expectOnMutant: "pass",
    needle: "WPrep(due, cx) = TurnStart -> WPrep(true, false)\n",
    replacement: "WPrep(due, cx) = ToolRequest?c?d -> WPrep(due, cx)\n            [] TurnStart -> WPrep(true, false)\n" },
  { name: "S9-prep 窗口派生", assertion: "MAIN [T= BAD_SPAWN_IN_PREP", expectOnMutant: "pass",
    needle: "WPrep(due, cx) = TurnStart -> WPrep(true, false)\n",
    replacement: "WPrep(due, cx) = AgentSpawn?a?r -> WPrep(due, cx)\n            [] TurnStart -> WPrep(true, false)\n" },
  { name: "S4-取消后仍可开轮", assertion: "MAIN [T= BAD_ROUNDSTART_AFTER_CANCEL", expectOnMutant: "pass",
    needle: "TCX(s) = SteerEnqueue -> TCX(true)\n",
    replacement: "TCX(s) = RoundStart -> TCX(s)\n      [] SteerEnqueue -> TCX(true)\n" },
  { name: "S8-不存在的任务也能被等待", assertion: "MAIN [T= BAD_WAIT_NO_TASK", expectOnMutant: "pass",
    needle: "ANone(a, r) = AgentSpawn.a!other(r) -> ARunning(a, other(r))\n",
    replacement: "ANone(a, r) = TaskWait.a?rr -> ANone(a, r)\n           [] AgentSpawn.a!other(r) -> ARunning(a, other(r))\n" },
  { name: "S8-同一任务重复发起等待", assertion: "MAIN [T= BAD_DOUBLE_WAIT", expectOnMutant: "pass",
    needle: "AWaitRunning(a, r) = AgentComplete.a.r -> AWaitDone(a, r)\n",
    replacement: "AWaitRunning(a, r) = TaskWait.a.r -> AWaitRunning(a, r)\n                  [] AgentComplete.a.r -> AWaitDone(a, r)\n" },
  { name: "S8-未settle即可交付", assertion: "MAIN [T= BAD_DELIVER_UNSETTLED", expectOnMutant: "pass",
    needle: "AWaitRunning(a, r) = AgentComplete.a.r -> AWaitDone(a, r)\n",
    replacement: "AWaitRunning(a, r) = TaskWaitDeliver.a.r -> ANone(a, r)\n                  [] AgentComplete.a.r -> AWaitDone(a, r)\n" },
  { name: "S8-未发起等待也能交付", assertion: "MAIN [T= BAD_DELIVER_NO_WAIT", expectOnMutant: "pass",
    needle: "ADone(a, r) = TaskWait.a.r -> AWaitDone(a, r)\n",
    replacement: "ADone(a, r) = TaskWaitDeliver.a.r -> ANone(a, r)\n           [] TaskWait.a.r -> AWaitDone(a, r)\n" },
  { name: "S8-轮带未决等待收束(running)", assertion: "MAIN [T= BAD_ROUNDEND_PENDING_WAIT", expectOnMutant: "pass",
    needle: "                  [] TurnCancel -> ARunningCx(a, r)\n",
    replacement: "                  [] RoundEnd -> AWaitRunning(a, r)\n                  [] TurnCancel -> ARunningCx(a, r)\n" },
  { name: "S8-轮带未决等待收束(settled)", assertion: "MAIN [T= BAD_ROUNDEND_PENDING_WAIT_SETTLED", expectOnMutant: "pass",
    needle: "AWaitDone(a, r) = TaskWaitDeliver.a.r -> ANone(a, r)\n",
    replacement: "AWaitDone(a, r) = RoundEnd -> AWaitDone(a, r)\n              [] TaskWaitDeliver.a.r -> ANone(a, r)\n" },
  { name: "S8S4-取消后仍可交付", assertion: "MAIN [T= BAD_DELIVER_AFTER_CANCEL", expectOnMutant: "pass",
    needle: "ADoneCx(a, r) = RoundEnd -> ADoneCx(a, r)\n",
    replacement: "ADoneCx(a, r) = TaskWaitDeliver.a.r -> ANoneCx(a, r)\n            [] RoundEnd -> ADoneCx(a, r)\n" },
  { name: "S8-未发起等待也可超时撤回", assertion: "MAIN [T= BAD_TIMEOUT_NO_WAIT", expectOnMutant: "pass",
    needle: "ARunning(a, r) = AgentComplete.a.r -> ADone(a, r)\n",
    replacement: "ARunning(a, r) = TaskWaitTimeout.a.r -> ARunning(a, r)\n              [] AgentComplete.a.r -> ADone(a, r)\n" },
  { name: "S8-已settle的等待也可超时撤回", assertion: "MAIN [T= BAD_TIMEOUT_SETTLED", expectOnMutant: "pass",
    needle: "AWaitDone(a, r) = TaskWaitDeliver.a.r -> ANone(a, r)\n",
    replacement: "AWaitDone(a, r) = TaskWaitTimeout.a.r -> AWaitDone(a, r)\n              [] TaskWaitDeliver.a.r -> ANone(a, r)\n" },
  { name: "S8-超时不充值义务", assertion: "MAIN [T= BAD_END_AFTER_TIMEOUT", expectOnMutant: "pass",
    needle: "             [] TaskWaitTimeout?a?r -> WRound(true, cx)\n",
    replacement: "             [] TaskWaitTimeout?a?r -> WRound(due, cx)\n" },
  { name: "S8-收窄:等待不可超时撤回", assertion: "MAIN [T= SCEN_WAIT_TIMEOUT", expectOnMutant: "refine-fail",
    needle: "                  [] TaskWaitTimeout.a.r -> ARunning(a, r)\n",
    replacement: "" },
  { name: "S10-超时撤回不核身份", assertion: "MAIN [T= BAD_TID_TIMEOUT", expectOnMutant: "pass",
    needle: "                  [] TaskWaitTimeout.a.r -> ARunning(a, r)\n",
    replacement: "                  [] TaskWaitTimeout.a?rr -> ARunning(a, r)\n" },
  { name: "S8-收窄:running 任务上不可发起等待", assertion: "MAIN [T= SCEN_ASYNC_WAIT", expectOnMutant: "refine-fail",
    needle: "              [] TaskWait.a.r -> AWaitRunning(a, r)\n",
    replacement: "" },
  { name: "S8-收窄:等待不再交付", assertion: "MAIN [T= SCEN_WAIT_SETTLED", expectOnMutant: "refine-fail",
    needle: "AWaitDone(a, r) = TaskWaitDeliver.a.r -> ANone(a, r)\n              [] TurnCancel -> ADoneCx(a, r)\n",
    replacement: "AWaitDone(a, r) = TurnCancel -> ADoneCx(a, r)\n" },
  { name: "S9-收窄:done 结果不再 fold", assertion: "MAIN [T= SCEN_FOLD", expectOnMutant: "refine-fail",
    needle: "           [] AgentFold.a.r -> ANone(a, r)\n",
    replacement: "" },
  { name: "S3-收窄:收紧后不再能重批", assertion: "MAIN [T= SCEN_REAPPROVE", expectOnMutant: "refine-fail",
    needle: "                        [] ToolApprove.c -> CAllowed(c, true, true)\n",
    replacement: "" },
  { name: "S9-收窄:取消不放弃交付义务", assertion: "MAIN [T= SCEN_CANCEL", expectOnMutant: "refine-fail",
    needle: "            [] ((not due) or cx) & TurnEnd -> WPrep(false, false)\n",
    replacement: "            [] (not due) & TurnEnd -> WPrep(false, false)\n" },
  { name: "S9-收窄:义务已清也不许正常收束", assertion: "MAIN [T= SCEN_APPROVE", expectOnMutant: "refine-fail",
    needle: "            [] ((not due) or cx) & TurnEnd -> WPrep(false, false)\n",
    replacement: "            [] cx & TurnEnd -> WPrep(false, false)\n" },
  { name: "进展性-取消收束后回合无法结束", assertion: "NoAdversary :[deadlock free [F]]", expectOnMutant: "deadlock",
    needle: "\n            [] TurnEnd -> CIdle(c)\n",
    replacement: "\n" },
  { name: "S10-派生不轮替身份", assertion: "MAIN [T= BAD_TID_SPAWN_REPEAT", expectOnMutant: "pass",
    needle: "ANone(a, r) = AgentSpawn.a!other(r) -> ARunning(a, other(r))\n",
    replacement: "ANone(a, r) = AgentSpawn.a?rr -> ARunning(a, rr)\n" },
  { name: "S10-完成不核身份", assertion: "MAIN [T= BAD_TID_COMPLETE", expectOnMutant: "pass",
    needle: "ARunning(a, r) = AgentComplete.a.r -> ADone(a, r)\n",
    replacement: "ARunning(a, r) = AgentComplete.a?rr -> ADone(a, r)\n" },
  { name: "S10-发起等待不核身份(running)", assertion: "MAIN [T= BAD_TID_WAIT", expectOnMutant: "pass",
    needle: "              [] TaskWait.a.r -> AWaitRunning(a, r)\n",
    replacement: "              [] TaskWait.a?rr -> AWaitRunning(a, r)\n" },
  { name: "S10-发起等待不核身份(done)", assertion: "MAIN [T= BAD_TID_WAIT_DONE", expectOnMutant: "pass",
    needle: "ADone(a, r) = TaskWait.a.r -> AWaitDone(a, r)\n",
    replacement: "ADone(a, r) = TaskWait.a?rr -> AWaitDone(a, r)\n" },
  { name: "S10-等待中完成不核身份", assertion: "MAIN [T= BAD_TID_COMPLETE_WAITING", expectOnMutant: "pass",
    needle: "AWaitRunning(a, r) = AgentComplete.a.r -> AWaitDone(a, r)\n",
    replacement: "AWaitRunning(a, r) = AgentComplete.a?rr -> AWaitDone(a, r)\n" },
  { name: "S10-交付不核身份", assertion: "MAIN [T= BAD_TID_DELIVER", expectOnMutant: "pass",
    needle: "AWaitDone(a, r) = TaskWaitDeliver.a.r -> ANone(a, r)\n",
    replacement: "AWaitDone(a, r) = TaskWaitDeliver.a?rr -> ANone(a, r)\n" },
  { name: "S10-fold 不核身份", assertion: "MAIN [T= BAD_TID_FOLD", expectOnMutant: "pass",
    needle: "           [] AgentFold.a.r -> ANone(a, r)\n",
    replacement: "           [] AgentFold.a?rr -> ANone(a, r)\n" },
  { name: "S10-取消路径完成不核身份", assertion: "MAIN [T= BAD_TID_COMPLETE_CANCEL", expectOnMutant: "pass",
    needle: "ARunningCx(a, r) = AgentComplete.a.r -> ADoneCx(a, r)\n",
    replacement: "ARunningCx(a, r) = AgentComplete.a?rr -> ADoneCx(a, r)\n" },
  { name: "S11-idle fold", assertion: "MAIN [T= BAD_FOLD_IDLE", expectOnMutant: "pass",
    needle: "AIdleDone(a, r) = TurnStart -> AWakeDone(a, r)\n",
    replacement: "AIdleDone(a, r) = AgentFold.a.r -> AIdle(a, r)\n               [] TurnStart -> AWakeDone(a, r)\n" },
  { name: "S11-正常收束带走未 fold 的 done", assertion: "MAIN [T= BAD_END_PENDING_DONE", expectOnMutant: "pass",
    needle: "ADone(a, r) = TaskWait.a.r -> AWaitDone(a, r)\n",
    replacement: "ADone(a, r) = TurnEnd -> AIdleDone(a, r)\n           [] TaskWait.a.r -> AWaitDone(a, r)\n" },
  { name: "S10-idle 完成不核身份", assertion: "MAIN [T= BAD_TID_COMPLETE_IDLE", expectOnMutant: "pass",
    needle: "AIdleRunning(a, r) = AgentComplete.a.r -> AIdleDone(a, r)\n",
    replacement: "AIdleRunning(a, r) = AgentComplete.a?rr -> AIdleDone(a, r)\n" },
  { name: "S11-收窄:idle 完成不被承认", assertion: "MAIN [T= SCEN_TASK_SURVIVES_END", expectOnMutant: "refine-fail",
    needle: "AIdleRunning(a, r) = AgentComplete.a.r -> AIdleDone(a, r)\n",
    replacement: "AIdleRunning(a, r) = STOP\n                  " },
  { name: "S11-收窄:存活任务不被下一回合承接", assertion: "MAIN [T= SCEN_ADOPT", expectOnMutant: "refine-fail",
    needle: "                  [] TurnStart -> ARunning(a, r)\n",
    replacement: "" },
  { name: "S11-收窄:唤醒回合不承接 done", assertion: "MAIN [T= SCEN_TASK_SURVIVES_END", expectOnMutant: "refine-fail",
    needle: "AIdleDone(a, r) = TurnStart -> AWakeDone(a, r)\n",
    replacement: "AIdleDone(a, r) = STOP\n" },
  { name: "S11-收窄:取消不保留 done", assertion: "MAIN [T= SCEN_CANCEL_DONE_CARRYOVER", expectOnMutant: "refine-fail",
    needle: "ADoneCx(a, r) = RoundEnd -> ADoneCx(a, r)\n            [] TurnEnd -> AIdleDone(a, r)\n",
    replacement: "ADoneCx(a, r) = RoundEnd -> ADoneCx(a, r)\n" },
  { name: "S11-收窄:取消窗口完成不保留", assertion: "MAIN [T= SCEN_CANCEL_WINDOW_COMPLETE", expectOnMutant: "refine-fail",
    needle: "ARunningCx(a, r) = AgentComplete.a.r -> ADoneCx(a, r)\n",
    replacement: "ARunningCx(a, r) = AgentComplete.a.r -> ANoneCx(a, r)\n" },
  { name: "S12-未执行的调用也能交接后台", assertion: "MAIN [T= BAD_BG_BEFORE_EXEC", expectOnMutant: "pass",
    needle: "CAllowed(c, false, fresh) = fresh & ToolExecStart.c -> CExecuting(c)\n",
    replacement: "CAllowed(c, false, fresh) = ToolBackground.c?a?r -> CExecutingBg(c)\n                         [] fresh & ToolExecStart.c -> CExecuting(c)\n" },
  { name: "S12-回执之后仍能交接后台", assertion: "MAIN [T= BAD_BG_AFTER_END", expectOnMutant: "pass",
    needle: "CDone(c) = ToolSettle.c -> CUnused(c)\n",
    replacement: "CDone(c) = ToolBackground.c?a?r -> CDone(c)\n        [] ToolSettle.c -> CUnused(c)\n" },
  { name: "S12-同一调用可反复交接后台", assertion: "MAIN [T= BAD_BG_TWICE", expectOnMutant: "pass",
    needle: "CExecutingBg(c) = ToolExecEnd.c -> CDone(c)\n",
    replacement: "CExecutingBg(c) = ToolBackground.c?a?r -> CExecutingBg(c)\n               [] ToolExecEnd.c -> CDone(c)\n" },
  { name: "S12-交接后台不轮替身份", assertion: "MAIN [T= BAD_BG_STALE_ID", expectOnMutant: "pass",
    needle: "           [] ToolBackground?c!a!other(r) -> ARunning(a, other(r))\n",
    replacement: "           [] ToolBackground?c!a?rr -> ARunning(a, rr)\n" },
  { name: "S12-已 settle 未取走的槽位可被交接覆盖", assertion: "MAIN [T= BAD_BG_SLOT_DONE", expectOnMutant: "pass",
    needle: "ADone(a, r) = TaskWait.a.r -> AWaitDone(a, r)\n",
    replacement: "ADone(a, r) = ToolBackground?c!a!other(r) -> ARunning(a, other(r))\n           [] TaskWait.a.r -> AWaitDone(a, r)\n" },
  { name: "S12-交接后台不受并发上限约束", assertion: "MAIN [T= BAD_BG_OVERCAP", expectOnMutant: "pass",
    needle: "           [] (n < MaxLiveAgents) & ToolBackground?c?a?r -> CAPACITY(n + 1)\n",
    replacement: "           [] ToolBackground?c?a?r -> CAPACITY(n + 1)\n" },
  { name: "S12-收窄:执行中的调用不可交接后台", assertion: "MAIN [T= SCEN_BG_TIMEOUT", expectOnMutant: "refine-fail",
    needle: "             [] ToolBackground.c?a?r -> CExecutingBg(c)\n",
    replacement: "" },
  { name: "S12-收窄:空任务槽不接收交接", assertion: "MAIN [T= SCEN_BG_WAIT", expectOnMutant: "refine-fail",
    needle: "           [] ToolBackground?c!a!other(r) -> ARunning(a, other(r))\n",
    replacement: "" },
];

function checkCspMutants() {
  const cspText = readFileSync(cspPath, "utf8");
  const work = mkdtempSync(path.join(os.tmpdir(), "formal-csp-mutants-"));
  try {
    for (const mutant of MUTANTS_CSP) {
      const found = occurrences(cspText, mutant.needle);
      if (found !== 1) {
        fail(`CSP 变异「${mutant.name}」的定位串出现 ${found} 次（应恰好 1 次）——协议正文变了，先更新 MUTANTS_CSP`);
      }
      const mutantPath = path.join(work, "AgentKernelMutant.csp");
      writeFileSync(mutantPath, cspText.replace(mutant.needle, mutant.replacement));
      const result = cspAssert([mutant.assertion], mutantPath);
      if (mutant.expectOnMutant === "pass") {
        expectCspPass(result, `CSP 变异「${mutant.name}」应使 ${mutant.assertion} 转为接受`);
      } else if (mutant.expectOnMutant === "refine-fail") {
        expectCspRefinementRefused(result, `CSP 变异「${mutant.name}」应使 ${mutant.assertion} 被拒`);
      } else if (mutant.expectOnMutant === "deadlock") {
        expectCspDeadlockFound(result, `CSP 变异「${mutant.name}」应制造死锁`);
      } else {
        fail(`CSP 变异「${mutant.name}」的 expectOnMutant 未知：${mutant.expectOnMutant}`);
      }
      log(`CSP 变异自检通过：「${mutant.name}」被抓住`);
    }
    log(`CSP 变异矩阵通过：${MUTANTS_CSP.length} 个协议变异全部被指定断言翻转抓住`);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}


// ---------------------------------------------------------------------------
// Stage 5: kernel trace agreement via TLA replay and CSP refinement
// ---------------------------------------------------------------------------

function exportTraces() {
  rmSync(traceDir, { recursive: true, force: true });
  mkdirSync(traceDir, { recursive: true });
  log("运行 agent-kernel 一致性测试并导出 trace……");
  const result = spawnSync(
    process.execPath,
    [path.join(scriptDir, "cargo-test.mjs"), "-p", "mework-agent-kernel"],
    {
      cwd: repoRoot,
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, AGENT_KERNEL_TRACE_DIR: traceDir },
      encoding: "utf8",
      timeout: 600_000,
    },
  );
  if (result.status !== 0) {
    fail(`agent-kernel 测试失败：\n${tail(result.stdout, 3000)}\n${tail(result.stderr)}`);
  }
}

function replayOne(file) {
  return runProbcli(
    [tlaPath, ...javaPathArgs(), "-trace_replay", "json", file, "-strict"],
    { timeout: 300_000 },
  );
}

function replayTraces() {
  const entries = readdirSync(traceDir).filter((name) => name.endsWith(".prob2trace"));
  const accepted = entries.filter((name) => !name.endsWith(".refused.prob2trace"));
  const refused = entries.filter((name) => name.endsWith(".refused.prob2trace"));
  if (accepted.length === 0 || refused.length === 0) {
    fail(`trace 导出不完整：accepted=${accepted.length} refused=${refused.length}`);
  }
  for (const name of accepted) {
    expectPerfectReplay(replayOne(path.join(traceDir, name)), `trace ${name}`);
  }
  log(`trace 回放通过：${accepted.length} 条接受序列全部完美回放`);

  // A rejected probe requires two checks: its prefix must replay perfectly and
  // the complete probe must be refused, localizing failure to the final event.
  const prefixDir = mkdtempSync(path.join(os.tmpdir(), "formal-prefix-"));
  try {
    for (const name of refused) {
      const probe = JSON.parse(readFileSync(path.join(traceDir, name), "utf8"));
      if (!Array.isArray(probe.transitionList) || probe.transitionList.length < 2) {
        fail(`拒绝探针 ${name} 形状异常：transitionList 过短`);
      }
      const prefix = { transitionList: probe.transitionList.slice(0, -1) };
      const prefixFile = path.join(prefixDir, name.replace(/\.refused\.prob2trace$/, ".prefix.prob2trace"));
      writeFileSync(prefixFile, JSON.stringify(prefix));
      expectPerfectReplay(replayOne(prefixFile), `探针前缀 ${name}`);
      expectReplayRefused(replayOne(path.join(traceDir, name)), `拒绝探针 ${name}`);
    }
  } finally {
    rmSync(prefixDir, { recursive: true, force: true });
  }
  log(`拒绝探针通过：${refused.length} 个内核拒绝的事件，前缀可回放且恰在末事件被模型拒绝`);
}

// ---------------------------------------------------------------------------
// Trace to CSP: mechanically translate exported events into trace processes for
// MAIN [T= membership. Translation is fail-closed for unknown names, absent
// parameters, and invalid parameter shapes.
// ---------------------------------------------------------------------------

const CSP_CALL_EVENTS = new Set([
  "ToolAllow", "ToolApprove", "ToolDeny", "ToolExecStart", "ToolExecEnd", "ToolSettle",
]);
const CSP_AGENT_EVENTS = new Set([
  "AgentSpawn", "AgentComplete", "AgentFold",
  "TaskWait", "TaskWaitDeliver", "TaskWaitTimeout",
]);
const CSP_BARE_EVENTS = new Set([
  "TurnStart", "TurnCancel", "TurnEnd", "ContextEdit", "PolicyTighten",
  "RoundStart", "RoundEnd", "SteerEnqueue", "SteerJoin",
]);

function cspEventText(transition, file) {
  const name = transition.name;
  const params = transition.params ?? {};
  const slot = (key) => {
    if (!/^\d+$/.test(params[key] ?? "")) {
      fail(`trace ${file}：事件 ${name} 的参数 ${key} 形状异常：${JSON.stringify(params)}`);
    }
    return params[key];
  };
  if (name === "ToolRequest") {
    if (!["TRUE", "FALSE"].includes(params.d)) {
      fail(`trace ${file}：ToolRequest 的 d 参数形状异常：${JSON.stringify(params)}`);
    }
    return `ToolRequest.${slot("c")}.${params.d === "TRUE" ? "true" : "false"}`;
  }
  if (name === "ToolBackground") {
    // The only event that names a call slot and a task slot at once, so it fits
    // none of the three sets above.
    return `ToolBackground.${slot("c")}.${slot("a")}.${slot("r")}`;
  }
  if (CSP_CALL_EVENTS.has(name)) return `${name}.${slot("c")}`;
  if (CSP_AGENT_EVENTS.has(name)) return `${name}.${slot("a")}.${slot("r")}`;
  if (CSP_BARE_EVENTS.has(name)) return name;
  fail(`trace ${file}：未知事件名 ${name}`);
}

function cspTraceProcess(defName, transitionList, file) {
  if (!Array.isArray(transitionList) || transitionList[0]?.name !== "$initialise_machine") {
    fail(`trace ${file} 形状异常：首项必须是 $initialise_machine`);
  }
  const events = transitionList.slice(1).map((t) => cspEventText(t, file));
  if (events.length === 0) return `${defName} = STOP`;
  return `${defName} = ${events.join("\n  -> ")}\n  -> STOP`;
}

function cspDefName(prefix, fileName) {
  return `${prefix}_${fileName.replace(/\.(refused\.)?prob2trace$/, "").replaceAll(/[^A-Za-z0-9]/g, "_")}`;
}

function replayCspTraces() {
  const entries = readdirSync(traceDir).filter((name) => name.endsWith(".prob2trace"));
  const accepted = entries.filter((name) => !name.endsWith(".refused.prob2trace"));
  const refused = entries.filter((name) => name.endsWith(".refused.prob2trace"));
  if (accepted.length === 0 || refused.length === 0) {
    fail(`trace 导出不完整：accepted=${accepted.length} refused=${refused.length}`);
  }
  const base = readFileSync(cspPath, "utf8");
  const defs = [];
  const passAssertions = [];
  const probes = [];
  for (const name of accepted) {
    const trace = JSON.parse(readFileSync(path.join(traceDir, name), "utf8"));
    const def = cspDefName("KTRACE", name);
    defs.push(cspTraceProcess(def, trace.transitionList, name));
    passAssertions.push(`MAIN [T= ${def}`);
  }
  for (const name of refused) {
    const probe = JSON.parse(readFileSync(path.join(traceDir, name), "utf8"));
    if (!Array.isArray(probe.transitionList) || probe.transitionList.length < 2) {
      fail(`拒绝探针 ${name} 形状异常：transitionList 过短`);
    }
    const prefixDef = cspDefName("KPREFIX", name);
    const probeDef = cspDefName("KPROBE", name);
    defs.push(cspTraceProcess(prefixDef, probe.transitionList.slice(0, -1), name));
    defs.push(cspTraceProcess(probeDef, probe.transitionList, name));
    passAssertions.push(`MAIN [T= ${prefixDef}`);
    probes.push({ name, assertion: `MAIN [T= ${probeDef}` });
  }
  const work = mkdtempSync(path.join(os.tmpdir(), "formal-csp-traces-"));
  try {
    const combined = path.join(work, "AgentKernelTraces.csp");
    writeFileSync(combined, `${base}\n\n-- 内核导出迹（机械生成，勿手改）\n\n${defs.join("\n\n")}\n`);
    expectCspPass(cspAssert(passAssertions, combined, 600_000), "CSP 迹回放（接受序列与探针前缀）");
    log(`CSP 迹回放通过：${accepted.length} 条接受序列与 ${refused.length} 条探针前缀全部被协议接受`);
    for (const probe of probes) {
      expectCspRefinementRefused(cspAssert([probe.assertion], combined), `CSP 拒绝探针 ${probe.name}`);
    }
    log(`CSP 拒绝探针通过：${probes.length} 个内核拒绝的事件同样被协议拒绝`);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

// ---------------------------------------------------------------------------

async function main() {
  const args = process.argv.slice(2);
  const skipMutants = args.includes("--no-mutants");
  const skipTraces = args.includes("--no-traces");
  const skipCsp = args.includes("--no-csp");
  const wide = args.includes("--wide");
  const known = ["--no-mutants", "--no-traces", "--no-csp", "--wide"];
  const unknown = args.filter((a) => !known.includes(a));
  if (unknown.length > 0) fail(`未知参数：${unknown.join(" ")}`);

  await ensureProb();
  if (!skipCsp) checkVocabulary();
  checkModel();
  if (wide) checkModelWide();
  if (!skipMutants) checkMutants();
  if (!skipCsp) {
    checkCspScenarios();
    checkCspComposition();
    if (!skipMutants) checkCspMutants();
  }
  if (!skipTraces) {
    exportTraces();
    checkAlphabet();
    replayTraces();
    if (!skipCsp) replayCspTraces();
  }
  log("全部通过");
}

main().catch((error) => fail(error?.stack ?? String(error)));

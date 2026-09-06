import type {
  ContextItem,
  Conversation,
  ConversationBranch
} from "../types";
import {
  isTaskUpdateFor,
  taskIdFromTaskCreate
} from "./orchestration";

type TimelineLane =
  | { kind: "conversation" }
  | { kind: "branch"; branchId: string };

export interface RemovedStateToolContext {
  lane: TimelineLane;
  index: number;
  context: ContextItem;
}

export interface StateToolDeletion {
  conversation: Conversation;
  removed: RemovedStateToolContext[];
  scope: "task-list" | "task";
}

/** Every call of the merged `todo` tool, whatever its action: deleting a task's
 * creating call takes its whole lane with it, and a read of that task is part of
 * that lane too. */
function isTaskStateCall(context: ContextItem): boolean {
  return context.kind === "tool" && context.toolName === "todo";
}

function visitContexts(
  contexts: ContextItem[],
  visit: (context: ContextItem, index: number) => void
): void {
  contexts.forEach(visit);
}

function conversationLanes(
  conversation: Conversation
): Array<{ lane: TimelineLane; contexts: ContextItem[] }> {
  return [
    { lane: { kind: "conversation" }, contexts: conversation.contexts },
    ...conversation.branches.map((branch) => ({
      lane: { kind: "branch" as const, branchId: branch.id },
      contexts: branch.contexts
    }))
  ];
}

function taskGetFor(context: ContextItem, taskId: string): boolean {
  return context.kind === "tool"
    && context.toolName === "todo"
    && context.input.action === "get"
    && typeof context.input.taskId === "string"
    && context.input.taskId.trim() === taskId;
}

function laneKey(lane: TimelineLane): string {
  return lane.kind === "conversation" ? "conversation" : `branch:${lane.branchId}`;
}

function samePlacement(
  left: RemovedStateToolContext,
  right: RemovedStateToolContext
): boolean {
  return laneKey(left.lane) === laneKey(right.lane)
    && left.context.id === right.context.id
    && left.index === right.index;
}

interface StateToolTopology {
  placements: RemovedStateToolContext[];
  effectivePrefix: (placement: RemovedStateToolContext) => RemovedStateToolContext[];
}

/**
 * Builds each stored lane's effective timeline. An inactive branch owns only
 * its suffix, so its prefix is inherited from the lane that owns the branch's
 * fork message. This lets state events bind to the exact create context they
 * can actually see instead of to a conversation-global task id.
 */
function stateToolTopology(conversation: Conversation): StateToolTopology {
  const lanes = conversationLanes(conversation);
  const ownByLane = new Map<string, RemovedStateToolContext[]>();
  const placementByContextId = new Map<string, RemovedStateToolContext>();
  for (const { lane, contexts } of lanes) {
    const placements: RemovedStateToolContext[] = [];
    visitContexts(contexts, (context, index) => {
      const placement = { lane, index, context };
      placements.push(placement);
      if (!placementByContextId.has(context.id)) {
        placementByContextId.set(context.id, placement);
      }
    });
    ownByLane.set(laneKey(lane), placements);
  }

  const branches = new Map(conversation.branches.map((branch) => [branch.id, branch]));
  const effectiveByLane = new Map<string, RemovedStateToolContext[]>();
  const resolving = new Set<string>();
  const effectiveLane = (lane: TimelineLane): RemovedStateToolContext[] => {
    const key = laneKey(lane);
    const cached = effectiveByLane.get(key);
    if (cached) return cached;
    const own = ownByLane.get(key) ?? [];
    if (lane.kind === "conversation" || resolving.has(key)) {
      effectiveByLane.set(key, own);
      return own;
    }

    resolving.add(key);
    const branch = branches.get(lane.branchId);
    const fork = branch ? placementByContextId.get(branch.forkContextId) : undefined;
    if (!fork || laneKey(fork.lane) === key) {
      resolving.delete(key);
      effectiveByLane.set(key, own);
      return own;
    }
    const parent = effectiveLane(fork.lane);
    const forkIndex = parent.findIndex((placement) => samePlacement(placement, fork));
    const effective = forkIndex >= 0
      ? [...parent.slice(0, forkIndex + 1), ...own]
      : own;
    resolving.delete(key);
    effectiveByLane.set(key, effective);
    return effective;
  };

  lanes.forEach(({ lane }) => effectiveLane(lane));
  const placements = lanes.flatMap(({ lane }) => ownByLane.get(laneKey(lane)) ?? []);
  return {
    placements,
    effectivePrefix: (placement) => {
      const effective = effectiveLane(placement.lane);
      const index = effective.findIndex((candidate) => samePlacement(candidate, placement));
      return index >= 0 ? effective.slice(0, index + 1) : [];
    }
  };
}

function taskListRootFor(
  topology: StateToolTopology,
  placement: RemovedStateToolContext
): RemovedStateToolContext | null {
  return topology.effectivePrefix(placement)
    .find((candidate) => taskIdFromTaskCreate(candidate.context)) ?? null;
}

function taskCreatorFor(
  topology: StateToolTopology,
  placement: RemovedStateToolContext,
  taskId: string
): RemovedStateToolContext | null {
  return topology.effectivePrefix(placement)
    .slice()
    .reverse()
    .find((candidate) => taskIdFromTaskCreate(candidate.context) === taskId) ?? null;
}

function filterContextTree(
  contexts: ContextItem[],
  removedIds: ReadonlySet<string>
): ContextItem[] {
  const next = contexts.filter((context) => !removedIds.has(context.id));
  return next.length === contexts.length ? contexts : next;
}

function contextsChanged(before: ContextItem[], after: ContextItem[]): boolean {
  if (before.length !== after.length) return true;
  return before.some((context, index) => context !== after[index]);
}

function removePlacements(
  conversation: Conversation,
  removed: RemovedStateToolContext[],
  now: string
): Conversation {
  const removedIds = new Set(removed.map((placement) => placement.context.id));
  const contexts = filterContextTree(conversation.contexts, removedIds);
  const branches = conversation.branches.map((branch) => {
    const nextContexts = filterContextTree(branch.contexts, removedIds);
    return contextsChanged(branch.contexts, nextContexts)
      ? { ...branch, contexts: nextContexts, updatedAt: now }
      : branch;
  });
  return {
    ...conversation,
    contexts,
    branches,
    updatedAt: now
  };
}

/**
 * Applies the Mework-specific ownership rule around Claude-style state tools.
 *
 * Claude exposes no list-create tool or list id, so the first successful
 * `todo create` on a branch's effective timeline is the implicit list root.
 * Removing it removes `todo` events only from lanes that inherit that exact
 * create context. Later `todo create` calls own only that task and related
 * updates/reads that resolve back to the same creator.
 */
export function deleteStateToolContext(
  conversation: Conversation,
  item: ContextItem,
  now: string
): StateToolDeletion | null {
  if (item.kind !== "tool") return null;
  const topology = stateToolTopology(conversation);
  const itemPlacement = topology.placements.find((placement) => (
    placement.context === item || placement.context.id === item.id
  ));
  if (!itemPlacement) return null;

  if (item.toolName === "todo" && item.input.action === "create") {
    const taskId = taskIdFromTaskCreate(item);
    if (!taskId) return null;
    const listRoot = taskListRootFor(topology, itemPlacement);
    const isListRoot = listRoot?.context.id === item.id;
    const removed = topology.placements.filter((placement) => {
      const { context } = placement;
      if (isListRoot) {
        return isTaskStateCall(context)
          && taskListRootFor(topology, placement)?.context.id === item.id;
      }
      return context.id === item.id
        || (
          (isTaskUpdateFor(context, taskId) || taskGetFor(context, taskId))
          && taskCreatorFor(topology, placement, taskId)?.context.id === item.id
        );
    });
    if (!removed.some((placement) => placement.context.id === item.id)) return null;
    return {
      conversation: removePlacements(conversation, removed, now),
      removed,
      scope: isListRoot ? "task-list" : "task"
    };
  }

  return null;
}

function laneContexts(
  conversation: Conversation,
  lane: TimelineLane
): ContextItem[] | null {
  if (lane.kind === "conversation") return conversation.contexts;
  return conversation.branches.find((branch) => branch.id === lane.branchId)?.contexts ?? null;
}

function placementGroupKey(placement: RemovedStateToolContext): string {
  return laneKey(placement.lane);
}

function allContextIds(conversation: Conversation): Set<string> {
  const ids = new Set<string>();
  for (const { contexts } of conversationLanes(conversation)) {
    visitContexts(contexts, (context) => ids.add(context.id));
  }
  return ids;
}

/**
 * Restores a cascade as one transaction. If any id or timeline container has
 * changed incompatibly, nothing is restored.
 */
export function restoreStateToolContexts(
  conversation: Conversation,
  removed: RemovedStateToolContext[],
  now: string
): Conversation | null {
  if (!removed.length) return conversation;
  const currentIds = allContextIds(conversation);
  if (removed.some((placement) => currentIds.has(placement.context.id))) return null;

  const grouped = new Map<string, RemovedStateToolContext[]>();
  for (const placement of removed) {
    const key = placementGroupKey(placement);
    grouped.set(key, [...(grouped.get(key) ?? []), placement]);
  }

  for (const placements of grouped.values()) {
    if (!laneContexts(conversation, placements[0].lane)) return null;
  }

  let contexts = conversation.contexts;
  let branches = conversation.branches;
  const touchedBranches = new Set<string>();
  for (const placements of grouped.values()) {
    const { lane } = placements[0];
    const roots = lane.kind === "conversation"
      ? contexts
      : branches.find((branch) => branch.id === lane.branchId)!.contexts;
    const restored = [...roots];
    placements
      .slice()
      .sort((left, right) => left.index - right.index)
      .forEach(({ context, index }) => {
        restored.splice(Math.min(index, restored.length), 0, context);
      });
    const nextRoots = restored;
    if (lane.kind === "conversation") {
      contexts = nextRoots;
    } else {
      touchedBranches.add(lane.branchId);
      branches = branches.map((branch) => (
        branch.id === lane.branchId
          ? { ...branch, contexts: nextRoots, updatedAt: now }
          : branch
      ));
    }
  }

  return {
    ...conversation,
    contexts,
    branches: branches.map((branch: ConversationBranch) => (
      touchedBranches.has(branch.id) ? { ...branch, updatedAt: now } : branch
    )),
    updatedAt: now
  };
}

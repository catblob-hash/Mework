import { describe, expect, it } from "vitest";
import type { ContextItem } from "../types";
import {
  EMPTY_TIMELINE_PATCH,
  TimelineHistory,
  applyTimelinePatch,
  insertionPatch,
  invertTimelinePatch,
  placedContexts,
  timelineHistoryKey
} from "./timelineHistory";

const message = (id: string, content = id): ContextItem => ({
  id,
  kind: "user",
  content,
  createdAt: "2026-10-02T00:00:00.000Z"
});

const ids = (contexts: ContextItem[] | null) => contexts?.map((context) => context.id) ?? null;

describe("timeline patches", () => {
  it("takes contexts out and puts them back where they were", () => {
    const list = ["a", "b", "c", "d"].map((id) => message(id));
    const patch = { ...EMPTY_TIMELINE_PATCH, removed: placedContexts(list, new Set(["b", "c"])) };

    const deleted = applyTimelinePatch(list, patch);
    expect(ids(deleted)).toEqual(["a", "d"]);
    expect(ids(applyTimelinePatch(deleted!, invertTimelinePatch(patch)))).toEqual(["a", "b", "c", "d"]);
  });

  it("puts a context back after the neighbour it had, wherever that has moved since", () => {
    const list = ["a", "b", "c"].map((id) => message(id));
    const patch = { ...EMPTY_TIMELINE_PATCH, removed: placedContexts(list, new Set(["c"])) };
    // Two contexts went in above it after it was deleted, so its old index is wrong now.
    const later = [message("x"), message("y"), message("a"), message("b"), message("reply")];

    expect(ids(applyTimelinePatch(later, invertTimelinePatch(patch)))).toEqual(["x", "y", "a", "b", "c", "reply"]);
  });

  it("keeps what a model run appended between an edit and its undo", () => {
    const list = [message("a")];
    const inserted = insertionPatch(list, message("placed"), 1);
    const afterRun = [...applyTimelinePatch(list, inserted)!, message("reply")];

    expect(ids(applyTimelinePatch(afterRun, invertTimelinePatch(inserted)))).toEqual(["a", "reply"]);
  });

  it("rewrites a context in place in both directions", () => {
    const before = message("a", "old");
    const after = message("a", "new");
    const patch = { ...EMPTY_TIMELINE_PATCH, replaced: [{ before, after }] };

    expect(applyTimelinePatch([before, message("b")], patch)?.[0]).toBe(after);
    expect(applyTimelinePatch([after, message("b")], invertTimelinePatch(patch))?.[0]).toBe(before);
  });

  it("lands nowhere once nothing it touches is there to change", () => {
    const patch = { ...EMPTY_TIMELINE_PATCH, removed: placedContexts([message("gone")], new Set(["gone"])) };
    expect(applyTimelinePatch([message("other")], patch)).toBeNull();
  });
});

describe("TimelineHistory", () => {
  const entry = (label: string) => ({ patch: EMPTY_TIMELINE_PATCH, label });

  it("keeps each timeline's edits apart", () => {
    const history = new TimelineHistory();
    history.record("conv_a", entry("a1"));
    history.record("conv_b", entry("b1"));

    expect(history.nextUndo("conv_a")?.label).toBe("a1");
    expect(history.nextUndo("conv_b")?.label).toBe("b1");
    // A fork or a hand-over is a new id, and a new id has nothing on record.
    expect(history.nextUndo("conv_a_fork")).toBeNull();
  });

  it("moves an edit between undo and redo, and a new edit ends the redo", () => {
    const history = new TimelineHistory();
    history.record("conv", entry("first"));
    history.record("conv", entry("second"));

    history.undone("conv");
    expect(history.nextUndo("conv")?.label).toBe("first");
    expect(history.nextRedo("conv")?.label).toBe("second");
    history.redone("conv");
    expect(history.nextUndo("conv")?.label).toBe("second");
    expect(history.nextRedo("conv")).toBeNull();

    history.undone("conv");
    history.record("conv", entry("third"));
    expect(history.nextRedo("conv")).toBeNull();
  });

  it("shares one budget across timelines and lets the oldest edit go first", () => {
    const history = new TimelineHistory(3);
    history.record("conv_a", entry("a1"));
    history.record("conv_b", entry("b1"));
    history.record("conv_a", entry("a2"));
    history.record("conv_b", entry("b2"));

    expect(history.size).toBe(3);
    history.undone("conv_a");
    history.undone("conv_a");
    // "a1" was the oldest of all, so it went; "a2" is still there to undo and redo.
    expect(history.nextUndo("conv_a")).toBeNull();
    expect(history.nextRedo("conv_a")?.label).toBe("a2");
    expect(history.nextUndo("conv_b")?.label).toBe("b2");
  });

  it("counts what is waiting to be redone against the budget too", () => {
    const history = new TimelineHistory(2);
    history.record("conv_a", entry("a1"));
    history.undone("conv_a");
    history.record("conv_b", entry("b1"));
    history.record("conv_b", entry("b2"));

    expect(history.size).toBe(2);
    expect(history.nextRedo("conv_a")).toBeNull();
  });

  it("forgets a timeline outright", () => {
    const history = new TimelineHistory();
    history.record("conv", entry("a"));
    history.forget("conv");
    expect(history.nextUndo("conv")).toBeNull();
    expect(history.size).toBe(0);
  });
});

describe("timelineHistoryKey", () => {
  const press = (key: string, modifiers: Partial<Record<"ctrlKey" | "metaKey" | "altKey" | "shiftKey", boolean>> = {}) => ({
    key,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    ...modifiers
  });

  it("reads Ctrl+Z as undo and Ctrl+X as redo, and nothing else", () => {
    expect(timelineHistoryKey(press("z", { ctrlKey: true }))).toBe("undo");
    expect(timelineHistoryKey(press("X", { ctrlKey: true }))).toBe("redo");
    expect(timelineHistoryKey(press("z"))).toBeNull();
    expect(timelineHistoryKey(press("z", { ctrlKey: true, shiftKey: true }))).toBeNull();
    expect(timelineHistoryKey(press("c", { ctrlKey: true }))).toBeNull();
  });
});

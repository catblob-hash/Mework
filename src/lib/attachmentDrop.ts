import { useEffect, useId, useRef, useState, useSyncExternalStore } from "react";
import { isTauriRuntime } from "./backend";
import {
  dragItemFromMediaType,
  dragItemFromProbe,
  rejectionsForDragItems,
  type AttachmentRejection,
  type DragItem
} from "./fileAttachments";
import { probeDroppedPaths, readDroppedFile } from "./runtime";

/**
 * Files dragged onto the window, and which drop zone is under them.
 *
 * In the desktop app the operating system's drag never reaches the page: Tauri's
 * native handler takes it and reports paths, so the host can say — while the
 * drag is still in the air — whether each path is a folder, and what the file
 * starts with. That is what lets a zone refuse a folder or an archive before
 * anything is dropped. The host only answers for paths its own window saw in the
 * drag, and only reads back what was actually dropped.
 *
 * In a plain browser (browser development) the page's own drag events arrive
 * instead, and they carry only media types; a zone decides what it can from
 * those and the drop settles the rest.
 */

export interface DropZoneOptions {
  /** Whether the message this zone feeds can take images. */
  imageInput: boolean;
  disabled?: boolean;
  onDrop: (files: File[], preRejected: AttachmentRejection[]) => void;
}

interface Zone {
  element: HTMLElement | null;
  options: DropZoneOptions;
}

export interface DragSnapshot {
  /** A file drag is over the window. */
  active: boolean;
  /** What is being dragged; `null` while the host is still looking. */
  items: readonly DragItem[] | null;
  /** The zone under the pointer, if any. */
  zoneId: string | null;
}

const IDLE: DragSnapshot = { active: false, items: null, zoneId: null };
const ZONE_ATTRIBUTE = "data-attachment-drop-zone";

const zones = new Map<string, Zone>();
const listeners = new Set<() => void>();
let snapshot: DragSnapshot = IDLE;
/** Bumped on every native enter so a slow probe cannot land on a later drag. */
let dragGeneration = 0;

function publish(next: DragSnapshot): void {
  if (
    next.active === snapshot.active
    && next.items === snapshot.items
    && next.zoneId === snapshot.zoneId
  ) return;
  snapshot = next;
  for (const listener of [...listeners]) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  ensureListening();
  return () => listeners.delete(listener);
}

function zoneIdAt(element: Element | null): string | null {
  const zone = element?.closest(`[${ZONE_ATTRIBUTE}]`);
  const id = zone?.getAttribute(ZONE_ATTRIBUTE) ?? null;
  if (!id) return null;
  const registered = zones.get(id);
  return registered && !registered.options.disabled ? id : null;
}

/**
 * Where a native drag position lands in the page.
 *
 * macOS reports points from the top-left of the web view, which are CSS pixels;
 * Windows reports physical pixels of the client area.
 */
function zoneIdAtNativePosition(position: { x: number; y: number } | undefined): string | null {
  if (!position || typeof document === "undefined") return null;
  const mac = typeof navigator !== "undefined" && navigator.platform.startsWith("Mac");
  const scale = mac ? 1 : window.devicePixelRatio || 1;
  return zoneIdAt(document.elementFromPoint(position.x / scale, position.y / scale));
}

let listening = false;

function ensureListening(): void {
  if (listening || typeof window === "undefined") return;
  listening = true;
  if (isTauriRuntime()) {
    void listenNative();
  } else {
    listenBrowser();
  }
}

type NativeDragEvent =
  | { type: "enter"; paths: string[]; position: { x: number; y: number } }
  | { type: "over"; position: { x: number; y: number } }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" };

async function listenNative(): Promise<void> {
  try {
    const { getCurrentWebview } = await import("@tauri-apps/api/webview");
    await getCurrentWebview().onDragDropEvent((event) => handleNative(event.payload as NativeDragEvent));
  } catch {
    // Without the event there is no native drop to take; paste and the picker still work.
    listening = false;
  }
}

function handleNative(event: NativeDragEvent): void {
  if (event.type === "enter") {
    // A drag that carries no files (text, a link) is not ours to show anything for.
    if (!event.paths.length) {
      publish(IDLE);
      return;
    }
    const generation = ++dragGeneration;
    publish({ active: true, items: null, zoneId: zoneIdAtNativePosition(event.position) });
    void probeDroppedPaths(event.paths).then(
      (probes) => {
        if (generation !== dragGeneration || !snapshot.active) return;
        publish({ ...snapshot, items: probes.map(dragItemFromProbe) });
      },
      () => {
        if (generation !== dragGeneration || !snapshot.active) return;
        publish({ ...snapshot, items: event.paths.map((path) => ({ name: basename(path), verdict: "unknown", path })) });
      }
    );
    return;
  }
  if (event.type === "over") {
    if (!snapshot.active) return;
    publish({ ...snapshot, zoneId: zoneIdAtNativePosition(event.position) });
    return;
  }
  if (event.type === "leave") {
    publish(IDLE);
    return;
  }
  const zoneId = zoneIdAtNativePosition(event.position);
  const items = snapshot.items;
  const generation = dragGeneration;
  publish(IDLE);
  const zone = zoneId ? zones.get(zoneId) : undefined;
  if (!zone || !event.paths.length) return;
  void (async () => {
    // A drop can outrun the probe; ask again rather than read blind.
    const known = items ?? await probeDroppedPaths(event.paths).then(
      (probes) => probes.map(dragItemFromProbe),
      () => event.paths.map((path): DragItem => ({ name: basename(path), verdict: "unknown", path }))
    );
    if (generation !== dragGeneration) return;
    const imageInput = zone.options.imageInput;
    const preRejected = rejectionsForDragItems(known, imageInput);
    const readable = known.filter((item) => (
      item.path
      && (item.verdict === "pdf" || item.verdict === "text" || item.verdict === "unknown"
        || (item.verdict === "image" && imageInput))
    ));
    const files: File[] = [];
    for (const item of readable) {
      try {
        files.push(await readDroppedFile(item.path as string));
      } catch {
        preRejected.push({ name: item.name || undefined, reason: "failed" });
      }
    }
    zone.options.onDrop(files, preRejected);
  })();
}

function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function carriesFiles(event: DragEvent): boolean {
  const types = event.dataTransfer?.types;
  if (!types) return false;
  return Array.from(types).includes("Files");
}

function browserItems(transfer: DataTransfer): DragItem[] | null {
  const items = transfer.items;
  if (!items?.length) return null;
  return Array.from(items)
    .filter((item) => item.kind === "file")
    .map((item) => dragItemFromMediaType(item.type));
}

/**
 * The page's own drag events, for a browser without the native handler.
 *
 * Listening on the window (capture) rather than per zone is what keeps a drop
 * that misses every zone from navigating the page to the file.
 */
function listenBrowser(): void {
  let depth = 0;
  window.addEventListener("dragenter", (event) => {
    if (!carriesFiles(event)) return;
    depth += 1;
    publish({
      active: true,
      items: event.dataTransfer ? browserItems(event.dataTransfer) : null,
      zoneId: zoneIdAt(event.target instanceof Element ? event.target : null)
    });
  }, true);
  window.addEventListener("dragover", (event) => {
    if (!carriesFiles(event)) return;
    event.preventDefault();
    const zoneId = zoneIdAt(event.target instanceof Element ? event.target : null);
    const items = snapshot.items ?? (event.dataTransfer ? browserItems(event.dataTransfer) : null);
    publish({ active: true, items, zoneId });
    if (!event.dataTransfer) return;
    const zone = zoneId ? zones.get(zoneId) : undefined;
    const acceptable = zone && (items === null || items.some((item) => (
      item.verdict === "pdf" || item.verdict === "text" || item.verdict === "unknown"
      || (item.verdict === "image" && zone.options.imageInput)
    )));
    event.dataTransfer.dropEffect = acceptable ? "copy" : "none";
  }, true);
  window.addEventListener("dragleave", (event) => {
    if (!carriesFiles(event)) return;
    depth = Math.max(0, depth - 1);
    if (depth === 0 || event.relatedTarget === null) {
      depth = 0;
      publish(IDLE);
    }
  }, true);
  window.addEventListener("drop", (event) => {
    if (!event.dataTransfer || !(carriesFiles(event) || event.dataTransfer.files?.length)) return;
    depth = 0;
    event.preventDefault();
    const zoneId = zoneIdAt(event.target instanceof Element ? event.target : null);
    publish(IDLE);
    const zone = zoneId ? zones.get(zoneId) : undefined;
    if (!zone) return;
    const { files, preRejected } = browserDroppedFiles(event.dataTransfer);
    if (files.length || preRejected.length) zone.options.onDrop(files, preRejected);
  }, true);
}

/** A browser drop's files, with folders set aside: they arrive as empty `File`s otherwise. */
function browserDroppedFiles(transfer: DataTransfer): { files: File[]; preRejected: AttachmentRejection[] } {
  const items = transfer.items ? Array.from(transfer.items).filter((item) => item.kind === "file") : [];
  if (!items.length) return { files: Array.from(transfer.files ?? []), preRejected: [] };
  const files: File[] = [];
  const preRejected: AttachmentRejection[] = [];
  for (const item of items) {
    const entry = typeof item.webkitGetAsEntry === "function" ? item.webkitGetAsEntry() : null;
    const file = item.getAsFile();
    if (entry?.isDirectory) {
      preRejected.push({ name: entry.name || file?.name || undefined, reason: "directory" });
      continue;
    }
    if (file) files.push(file);
  }
  return { files, preRejected };
}

export interface DropZoneState {
  /** A file drag is somewhere over the window. */
  dragging: boolean;
  /** …and it is over this zone. */
  over: boolean;
  /** What is being dragged; `null` while it is still being looked at. */
  items: readonly DragItem[] | null;
}

/**
 * Makes an element a place files can be dropped, and reports the drag.
 *
 * Attach the returned `ref` to the element. The options are read at drop time,
 * so a zone whose model changes mid-drag judges the drop by the model it has
 * then.
 */
export function useAttachmentDropZone(
  options: DropZoneOptions
): DropZoneState & { ref: (element: HTMLElement | null) => void } {
  const id = useId();
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const [element, setElement] = useState<HTMLElement | null>(null);

  useEffect(() => {
    const zone: Zone = {
      element,
      get options() {
        return optionsRef.current;
      }
    };
    zones.set(id, zone);
    element?.setAttribute(ZONE_ATTRIBUTE, id);
    ensureListening();
    return () => {
      zones.delete(id);
      if (element?.getAttribute(ZONE_ATTRIBUTE) === id) element.removeAttribute(ZONE_ATTRIBUTE);
    };
  }, [id, element]);

  const current = useSyncExternalStore(subscribe, () => snapshot, () => IDLE);
  const disabled = Boolean(options.disabled);
  return {
    ref: setElement,
    dragging: current.active && !disabled,
    over: current.active && !disabled && current.zoneId === id,
    items: current.items
  };
}

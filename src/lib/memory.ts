import { invoke } from "./backend";

export type MemoryTier = "global" | "project";

export interface MemoryFileSummary {
  name: string;
  path: string;
  description: string | null;
}

export interface MemoryTierOverview {
  tier: MemoryTier;
  available: boolean;
  rootPath: string | null;
  instructions: MemoryFileSummary | null;
  index: MemoryFileSummary | null;
  documents: MemoryFileSummary[];
}

export interface MemoryOverview {
  global: MemoryTierOverview;
  project: MemoryTierOverview;
}

export interface MemoryFile {
  tier: MemoryTier;
  name: string;
  path: string;
  content: string;
}

function args(workspaceId: string | null, tier?: MemoryTier, name?: string) {
  return {
    workspaceId: workspaceId || null,
    ...(tier ? { tier } : {}),
    ...(name ? { name } : {})
  };
}

export function getMemoryOverview(workspaceId: string | null): Promise<MemoryOverview> {
  return invoke<MemoryOverview>("mework_memory_overview", { workspaceId: workspaceId || null });
}

export function readMemoryFile(workspaceId: string | null, tier: MemoryTier, name: string): Promise<MemoryFile> {
  return invoke<MemoryFile>("mework_memory_read_file", args(workspaceId, tier, name));
}

export function writeMemoryFile(workspaceId: string | null, tier: MemoryTier, name: string, content: string): Promise<MemoryFile> {
  return invoke<MemoryFile>("mework_memory_write_file", { ...args(workspaceId, tier, name), content });
}

export function deleteMemoryDocument(workspaceId: string | null, tier: MemoryTier, name: string): Promise<void> {
  return invoke<unknown>("mework_memory_delete_document", args(workspaceId, tier, name)).then(() => undefined);
}

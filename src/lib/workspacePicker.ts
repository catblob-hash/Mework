import { hasBackendRuntime, invoke } from "./backend";

export function hasNativeWorkspacePicker(): boolean {
  return hasBackendRuntime();
}

export async function pickWorkspaceDirectory(): Promise<string | null> {
  if (!hasNativeWorkspacePicker()) return null;
  return invoke<string | null>("pick_workspace_directory");
}

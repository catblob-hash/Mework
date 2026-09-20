import { invoke } from "./backend";

export type ProjectImportDecision = "allowed" | "denied";
export type ProjectImportTargetKind = "file" | "directory";

/**
 * Renderer-safe metadata for one external project-memory import decision.
 *
 * Canonical workspace/target paths and file contents deliberately have no
 * representation in this type. The adapter below copies only these fields
 * from the backend response, so later UI code cannot accidentally spread
 * private authorization material into the DOM.
 */
export interface ProjectImportTrustSummary {
  id: string;
  workspaceId: string;
  label: string;
  targetKind: ProjectImportTargetKind;
  decision: ProjectImportDecision;
  activeForCurrentWorkspace: boolean;
  createdAt: string;
  updatedAt: string;
}

const TRUST_RECORD_ID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function requireIdentifier(value: string, label: string): string {
  if (!value.trim() || Array.from(value).some((character) => character.charCodeAt(0) < 32)) {
    throw new Error(`${label} is invalid`);
  }
  return value;
}

function summaryFromUnknown(
  value: unknown,
  expectedWorkspaceId: string
): ProjectImportTrustSummary {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Project import trust backend returned an invalid record");
  }
  const record = value as Record<string, unknown>;
  const {
    id,
    workspaceId,
    label,
    targetKind,
    decision,
    activeForCurrentWorkspace,
    createdAt,
    updatedAt
  } = record;
  if (
    typeof id !== "string"
    || !TRUST_RECORD_ID_PATTERN.test(id)
    || typeof workspaceId !== "string"
    || workspaceId !== expectedWorkspaceId
    || typeof label !== "string"
    || !label.trim()
    || label.length > 256
    || Array.from(label).some((character) => character.charCodeAt(0) < 32)
    || (targetKind !== "file" && targetKind !== "directory")
    || (decision !== "allowed" && decision !== "denied")
    || typeof activeForCurrentWorkspace !== "boolean"
    || typeof createdAt !== "string"
    || !createdAt
    || typeof updatedAt !== "string"
    || !updatedAt
  ) {
    throw new Error("Project import trust backend returned unsafe metadata");
  }
  return {
    id,
    workspaceId,
    label,
    targetKind,
    decision,
    activeForCurrentWorkspace,
    createdAt,
    updatedAt
  };
}

/** Lists content-free decisions for exactly one host-owned workspace id. */
export async function listProjectImportTrust(
  workspaceId: string
): Promise<ProjectImportTrustSummary[]> {
  const exactWorkspaceId = requireIdentifier(workspaceId, "Project import workspace id");
  let raw: unknown;
  try {
    raw = await invoke<unknown>("project_memory_list_import_trust", {
      workspaceId: exactWorkspaceId
    });
  } catch {
    // Backend diagnostics may refer to private authorization state. Never
    // forward those strings into renderer UI or logs.
    throw new Error("Project import trust list failed");
  }
  if (!Array.isArray(raw)) {
    throw new Error("Project import trust backend returned an invalid list");
  }
  return raw.map((record) => summaryFromUnknown(record, exactWorkspaceId));
}

/**
 * Revokes one decision. The same external identity will require a new native
 * approval if a later project-memory discovery encounters it again.
 */
export function revokeProjectImportTrust(
  workspaceId: string,
  recordId: string
): Promise<void> {
  const exactWorkspaceId = requireIdentifier(workspaceId, "Project import workspace id");
  const exactRecordId = requireIdentifier(recordId, "Project import trust record id");
  if (!TRUST_RECORD_ID_PATTERN.test(exactRecordId)) {
    return Promise.reject(new Error("Project import trust record id is invalid"));
  }
  return invoke<unknown>("project_memory_revoke_import_trust", {
    workspaceId: exactWorkspaceId,
    recordId: exactRecordId
  }).then(
    () => undefined,
    () => {
      // Keep canonical target/workspace diagnostics outside renderer errors.
      throw new Error("Project import trust revoke failed");
    }
  );
}

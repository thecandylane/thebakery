// Pure target/model selection rules for the Studio composer — extracted from
// Chat.svelte so `studioState.check.ts` exercises the exact function the
// widget calls, not a second reimplementation of the same rules.

import type { AgentInfo, EffortLevel, ModelCatalog, ModelOption, SocketInfo } from "./node.ts";

export type TargetSelection = {
  /** The role id to send a turn to, or null if none is unambiguous. */
  selectedRoleId: string | null;
  canSend: boolean;
};

/**
 * Conversation targets are only configured agents whose assignment is
 * currently usable on this socket (`socket_ready`) — an unassigned or
 * unavailable role is never a target, only a configuration-surface entry.
 *
 * `previousRoleId` lets a caller keep an existing explicit choice across
 * projection refreshes instead of resetting it on every poll; pass null when
 * there is no prior selection to preserve.
 */
export function deriveTargetSelection(
  socket: SocketInfo | null,
  previousRoleId: string | null,
): TargetSelection {
  const ready = readyAgents(socket);
  if (ready.length === 0) return { selectedRoleId: null, canSend: false };
  if (ready.length === 1) return { selectedRoleId: ready[0].role_id, canSend: true };
  if (previousRoleId != null && ready.some((a) => a.role_id === previousRoleId)) {
    return { selectedRoleId: previousRoleId, canSend: true };
  }
  return { selectedRoleId: null, canSend: false };
}

export function readyAgents(socket: SocketInfo | null): AgentInfo[] {
  return socket?.agents.filter((a) => a.socket_ready) ?? [];
}

/**
 * The assignment picker's candidate models: node-wide `ModelOption`s
 * narrowed to this socket's own readiness (`SocketInfo.available_models`).
 * `ModelOption.node_available` alone cannot drive this — it says nothing
 * about whether ACP specifically is usable on THIS socket (e.g. its
 * workspace is missing from disk), so a broken socket must not offer ACP
 * even though the node has an `acp_cli` configured. Zero-config: a bare
 * workspace with no mise.toml is still ready — mise.toml is one optional
 * adapter, never a gate.
 */
export function deriveAvailableModels(
  socket: SocketInfo | null,
  modelOptions: ModelOption[],
): ModelOption[] {
  const readyIds = new Set((socket?.available_models ?? []).filter((m) => m.ready).map((m) => m.id));
  return modelOptions.filter((m) => readyIds.has(m.id));
}

/**
 * Which effort options a per-turn override control should offer (07d-A3D1
 * Phase D): the specific model variant the human picked for this turn if
 * they picked one, else the seat's own currently-assigned model — an
 * effort-only override (no model change) still needs somewhere to read
 * levels from. Empty when the catalog has nothing for the resolved id at
 * all (not found, or a genuinely empty `effort_levels`) — never a guess.
 */
export function effortLevelsForOverride(
  catalog: ModelCatalog | null,
  assignedModelId: string | null,
  overrideModelId: string,
): EffortLevel[] {
  if (!catalog || catalog.status !== "available") return [];
  const targetId = overrideModelId.trim() || assignedModelId;
  if (!targetId) return [];
  return catalog.models.find((m) => m.id === targetId)?.effort_levels ?? [];
}

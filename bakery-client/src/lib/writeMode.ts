// Write gate labels — node speaks snake_case; chrome speaks human.

export type WriteModeId = "manual" | "rules" | "auto";

/** Short chip for title/status: Manual / Rules / Auto */
export function writeModeLabel(mode: string | null | undefined): string {
  switch ((mode ?? "").toLowerCase()) {
    case "manual":
      return "Manual";
    case "rules":
      return "Rules";
    case "auto":
      return "Auto";
    default:
      return mode?.trim() ? mode : "—";
  }
}

/** Longer hint for titles/tooltips */
export function writeModeTitle(mode: string | null | undefined): string {
  switch ((mode ?? "").toLowerCase()) {
    case "manual":
      return "Manual — every file write needs Accept in Review";
    case "rules":
      return "Rules — only allow-listed globs in node.json auto-write; else Manual";
    case "auto":
      return "Auto — every proposal writes to disk (still evented as policy/auto)";
    default:
      return "Write mode unknown — check node connection";
  }
}

/** Broadcast so layout chrome updates when studio routes change mode. */
export const NODE_INFO_EVENT = "bakery:node-info";

export function publishNodeInfo(info: unknown): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent(NODE_INFO_EVENT, { detail: info }));
}

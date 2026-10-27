// The one `/` command registry for the agent composer.
//
// Membership rule, from the slice: a command exists here only if it invokes
// an operation Bakery already has, end to end. "Looks like a chat app should
// have it" is not a reason — an inert control is worse than an absent one,
// because the human plans around it. A command the thread's model cannot
// run has no row at all.
//
// Unavailable-right-now is different from unbacked: a real command whose
// precondition is not met stays visible with `disabledReason` set, because
// hiding it would imply Bakery cannot do it at all.

/** Everything the availability rules need. A plain value, deliberately not
 *  the whole snapshot — these rules are unit-tested without React. */
export type CommandContext = {
  streamId: string | null;
  /** This agent has a usable assignment (`socket_ready`). */
  canSend: boolean;
  assignedModelId: string | null;
  /** The seat's provider catalog resolved to at least one model variant. */
  hasOverrideModels: boolean;
  /** The resolved model reports at least one effort level. */
  hasEffortLevels: boolean;
  /** This socket has a worktree on disk to pick files from. */
  hasWorktree: boolean;
  interrupted: boolean;
  cancellable: boolean;
  /** How many models may be assigned to this seat on this socket. */
  assignableModels: number;
};

export type CommandId = "model" | "effort" | "file" | "plan" | "review" | "resume" | "stop" | "assign";

export type SlashCommand = {
  id: CommandId | `native:${string}`;
  /** What the human types after `/`. */
  name: string;
  label: string;
  hint: string;
  /** `argument` opens a picker; `action` fires immediately. */
  kind: "argument" | "action";
  /** Null when runnable. Otherwise the specific reason, shown in the menu —
   *  the command stays listed so its absence is never mistaken for Bakery
   *  not supporting it. */
  disabledReason: string | null;
};

/** Every backed command, in menu order, with availability resolved against
 *  `ctx`. Order is stable: frequency of use, not alphabetical, so muscle
 *  memory works. */
export function availableCommands(ctx: CommandContext): SlashCommand[] {
  const noStream = ctx.streamId == null ? "no session bound to this panel" : null;
  const notReady = ctx.canSend ? null : "this agent has no usable model assigned";

  return [
    {
      id: "model",
      name: "model",
      label: "Model for this turn",
      hint: "One-shot override; does not change the seat's assignment",
      kind: "argument",
      disabledReason:
        noStream ??
        notReady ??
        (ctx.assignedModelId == null
          ? "assign a model to this agent first"
          : ctx.hasOverrideModels
            ? null
            : "this provider does not enumerate model variants"),
    },
    {
      id: "effort",
      name: "effort",
      label: "Effort for this turn",
      hint: "One-shot override; levels come from the provider's catalog",
      kind: "argument",
      disabledReason:
        noStream ?? notReady ?? (ctx.hasEffortLevels ? null : "the selected model reports no effort levels"),
    },
    {
      id: "file",
      name: "file",
      label: "Attach a workspace file",
      hint: "Sends the file as context with the next instruction",
      kind: "argument",
      disabledReason: noStream ?? (ctx.hasWorktree ? null : "this conversation has no workspace on disk"),
    },
    {
      id: "plan",
      name: "plan",
      label: "Use Plan mode",
      hint: "Explain and use read-only tools; do not create edit proposals",
      kind: "action",
      // Mode is durable agent policy, so it remains useful before a model is
      // assigned; it will govern the first later turn on this stream.
      disabledReason: noStream,
    },
    {
      id: "review",
      name: "review",
      label: "Open Review",
      hint: "Focus the Review pane beside this conversation",
      kind: "action",
      disabledReason: noStream,
    },
    {
      id: "resume",
      name: "resume",
      label: "Resume unfinished turn",
      hint: "Continues a turn that failed, hit its step limit, or a node restart killed",
      kind: "action",
      disabledReason: noStream ?? (ctx.interrupted ? null : "no interrupted turn to resume"),
    },
    {
      id: "stop",
      name: "stop",
      label: "Stop this turn",
      hint: "Requests cancellation of the running turn",
      kind: "action",
      disabledReason: noStream ?? (ctx.cancellable ? null : "nothing is running"),
    },
    {
      id: "assign",
      name: "assign",
      label: "Assign this agent a model",
      hint: "Persistent seat assignment — not a per-turn override",
      kind: "argument",
      disabledReason: noStream ?? (ctx.assignableModels > 0 ? null : "no models are ready on this socket"),
    },
  ];
}

/** `/` menu matching. Returns null when the text is not a slash token at the
 *  very start of the composer — a `/` mid-sentence is just a slash, and a
 *  path like `/etc/hosts` stops matching as soon as it is clearly not one
 *  of ours. */
export function parseSlash(text: string): { query: string } | null {
  if (!text.startsWith("/")) return null;
  const rest = text.slice(1);
  // A space ends the command token; past that the argument is being typed.
  if (/\s/.test(rest)) return null;
  return { query: rest.toLowerCase() };
}

/** Prefix match on name, then label substring — narrow and predictable. */
export function filterCommands(commands: readonly SlashCommand[], query: string): SlashCommand[] {
  if (query === "") return [...commands];
  const q = query.toLowerCase();
  return commands.filter((c) => c.name.startsWith(q) || c.label.toLowerCase().includes(q));
}

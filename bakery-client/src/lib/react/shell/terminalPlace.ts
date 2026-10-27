// The room the terminal bar's second tab opens: the place the operator is
// in, read from what the screen shows. Pure, so `node` checks it
// (terminalPlace.check.ts); the shell reads it on every render, so the tab
// follows every move, between tabs, places and workspaces.

/** A room of the bar's: where its terminals open, and what the tab says. */
export type TerminalPlace = {
  /** The node stream its terminals are on: a thread's, or `project:<id>`. */
  stream: string;
  /** How a new one opens: in the thread's room; a project's as its own
   *  rules say (the node picks the mode on a `project:` stream). */
  mode: "room" | "host";
  kind: "project" | "workspace" | "thread";
  /** The place's own name: its project's, workspace's or thread's. */
  name: string;
};

/** The place on screen, or null where there is no room: no workspace (the
 *  Floor), or one with neither a project nor a thread in focus (the Baker's
 *  home page, a hand-made workspace) — the bar shows Host alone there. */
export function terminalPlace(on: {
  /** The workspace on screen. `orc`: an orc's workspace, by the orc's thread. */
  workspace: { name: string; orc?: string } | null;
  /** The live thread of the focused panel (a tool's, for one on a thread's worktree). */
  thread: { streamId: string; title: string } | null;
  /** The workspace's project. */
  project: { id: string; name: string } | null;
}): TerminalPlace | null {
  const { workspace, thread, project } = on;
  if (!workspace) return null;
  if (thread) {
    // A workspace's orc works on the workspace's own worktree: that room is the workspace.
    return workspace.orc === thread.streamId
      ? { stream: thread.streamId, mode: "room", kind: "workspace", name: workspace.name }
      : { stream: thread.streamId, mode: "room", kind: "thread", name: thread.title };
  }
  return project ? { stream: `project:${project.id}`, mode: "host", kind: "project", name: project.name } : null;
}

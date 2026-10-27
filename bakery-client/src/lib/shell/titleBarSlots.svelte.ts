// The title bar is the only row of chrome above a page. Pages and the
// workbench put their controls into it instead of stacking bars of their
// own: a component fills a slot while it is mounted and clears it on
// teardown. A snippet keeps its defining component's state and scoped
// styles, so the controls behave exactly as they would in place.

import type { Snippet } from "svelte";

export type TitleBarSlot = "crumb" | "tools" | "actions";

class TitleBarSlots {
  /** The name of what is open, after the tools — e.g. the open Baker
   *  thread's title. */
  crumb = $state<Snippet | null>(null);
  /** The workbench's panel controls. First in the bar. */
  tools = $state<Snippet | null>(null);
  /** The page's own actions, after the tools. */
  actions = $state<Snippet | null>(null);
}

export const titleBar = new TitleBarSlots();

/** Fills `slot` for the lifetime of the calling component. Call during
 *  component initialisation. The identity check keeps a page that is
 *  unmounting from clearing the slot its successor has already filled. */
export function useTitleBarSlot(slot: TitleBarSlot, snippet: Snippet): void {
  $effect(() => {
    titleBar[slot] = snippet;
    return () => {
      if (titleBar[slot] === snippet) titleBar[slot] = null;
    };
  });
}

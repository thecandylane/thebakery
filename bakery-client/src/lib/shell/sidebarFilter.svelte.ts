// The sidebar's status filter. Kept here rather than in the sidebar because
// the sidebar unmounts while it is hidden (and the compact drawer on every
// close); the filter should survive that, but not an app restart.

import type { ConversationStatus } from "$lib/node";

export type StatusFilter = "all" | ConversationStatus;

class SidebarFilter {
  value = $state<StatusFilter>("all");

  /** Picking the filter that is already on turns it off. */
  toggle(next: StatusFilter) {
    this.value = this.value === next ? "all" : next;
  }
}

export const sidebarFilter = new SidebarFilter();

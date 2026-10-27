import { redirect } from "@sveltejs/kit";

// 07d-A2: the Factory Floor landing moved to / (routes/+page.svelte absorbed
// FactoryFloor.svelte's live dashboard + the old root foyer's open/create/
// attach wizard). Bare /w is kept as a permanent redirect, not deleted —
// old links, bookmarks, and the rail's former "Sessions" target all pointed
// here.
export function load() {
  redirect(308, "/");
}

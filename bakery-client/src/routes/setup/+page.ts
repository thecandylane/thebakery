import { redirect } from "@sveltejs/kit";

// /setup moved into the control plane as Settings → Provider accounts (J9).
// Kept as a permanent redirect rather than deleted: the old path is in
// muscle memory, in the foyer footer link, and possibly in a bookmark.
export function load() {
  redirect(308, "/settings/providers");
}

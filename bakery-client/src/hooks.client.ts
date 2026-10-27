import type { HandleClientError } from "@sveltejs/kit";
import { installClientLog, reportError } from "$lib/clientLog";

installClientLog();

// A local desktop app has one operator and nothing to hide from them: a
// production build otherwise replaces every unexpected error with a bare
// "Internal Error", which says nothing about what broke. Show the real error
// and where it was thrown, keep it in the console, and report it to the node.
export const handleError: HandleClientError = ({ error, status, message }) => {
  console.error(error);
  if (status === 404) return { message };
  reportError("route", error);
  if (!(error instanceof Error)) return { message: String(error) };
  const where = (error.stack ?? "").split("\n").slice(0, 4).join(" ← ");
  return { message: `${error.name}: ${error.message}${where ? ` — ${where}` : ""}` };
};

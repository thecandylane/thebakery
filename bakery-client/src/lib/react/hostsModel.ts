// Hosts a thread's sandbox asked to reach (the project's network list):
// which ones are shared by anyone's buckets and sites, and the one
// `*.domain` pattern that would answer several asks at once. Pure and
// DOM-free: hostsModel.check.ts runs it on node.

/** Domains anyone can keep a bucket or a site under: allowing one lets an
 *  agent send data to a stranger's. */
const SHARED = [
  "storage.googleapis.com",
  "s3.amazonaws.com",
  "blob.core.windows.net",
  "web.app",
  "firebaseapp.com",
  "firebaseio.com",
  "cloudfunctions.net",
  "appspot.com",
  "r2.dev",
  "pages.dev",
  "workers.dev",
  "vercel.app",
  "netlify.app",
  "github.io",
  "herokuapp.com",
  "supabase.co",
];

const under = (host: string, domain: string) => host === domain || host.endsWith(`.${domain}`);

/** Whether a host, or a `*.domain` pattern, reaches a shared domain. */
export function sharedTenant(entry: string): boolean {
  const host = entry.toLowerCase();
  if (!host.startsWith("*.")) return SHARED.some((d) => under(host, d));
  const domain = host.slice(2);
  // `*.googleapis.com` covers storage.googleapis.com; `*.x.web.app` sits in web.app.
  return SHARED.some((d) => under(d, domain) || under(domain, d));
}

/** Whether an answer for `entry` (a host, or `*.domain`) answers an ask for
 *  `host`: the same host, or one under the domain (never the domain itself). */
export function covers(entry: string, host: string): boolean {
  const e = entry.toLowerCase();
  const h = host.toLowerCase();
  return e === h || (e.startsWith("*.") && h.endsWith(e.slice(1)));
}

/** A host's registrable part, near enough: its last two labels (three under
 *  a two-letter country's co/com/org/net/ac/gov). */
function parentOf(host: string): string | null {
  const labels = host.toLowerCase().split(".");
  const take = labels.length > 2 && labels.at(-1)!.length === 2 && ["co", "com", "org", "net", "ac", "gov"].includes(labels.at(-2)!) ? 3 : 2;
  return labels.length > take ? labels.slice(-take).join(".") : null;
}

/** The `*.domain` that would answer the most of these asks at once (two at
 *  least), and which ones it covers; null when no two share a domain. A
 *  pattern never covers the domain itself. */
export function patternFor(hosts: readonly string[]): { pattern: string; covers: string[] } | null {
  const by = new Map<string, string[]>();
  for (const host of hosts) {
    const parent = parentOf(host);
    if (parent) by.set(parent, [...(by.get(parent) ?? []), host]);
  }
  let best: [string, string[]] | null = null;
  for (const entry of by) if (entry[1].length >= 2 && (!best || entry[1].length > best[1].length)) best = entry;
  return best ? { pattern: `*.${best[0]}`, covers: best[1] } : null;
}

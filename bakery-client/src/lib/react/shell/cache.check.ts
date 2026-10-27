// node src/lib/react/shell/cache.check.ts: a panel's reply is kept across a
// restart, a secret one never is (and forgets what was kept), two loads of
// one key at once run once, and "updated N min ago" reads plainly.
export {};

const shelf = new Map<string, string>();
(globalThis as { localStorage?: unknown }).localStorage = {
  getItem: (k: string) => shelf.get(k) ?? null,
  setItem: (k: string, v: string) => void shelf.set(k, v),
  removeItem: (k: string) => void shelf.delete(k),
};

const { keep, kept, refresh, updatedAgo } = await import("./cache.ts");

let failed = 0;
const ok = (name: string, pass: boolean) => {
  console.log(`${pass ? "ok  " : "FAIL"} ${name}`);
  if (!pass) failed++;
};

keep("prs", { items: [{ number: 7 }] }, 1_000);
ok("kept at once", (kept("prs")?.data as { items: unknown[] }).items.length === 1 && kept("prs")?.at === 1_000);
const restarted: typeof import("../../kept.ts") = await import(`../../kept.ts?${"restart"}`);
ok("kept across a restart, from this window's storage", (restarted.kept("prs")?.data as { items: unknown[] } | undefined)?.items.length === 1);

keep("diff", { diff: "x", secret: false });
ok("a reply that isn't secret is kept", kept("diff") !== null);
keep("diff", { diff: "TOKEN=…", secret: true });
ok("a secret reply is never kept, and forgets the one before", kept("diff") === null && [...shelf.keys()].every((k) => !k.endsWith(":diff")));

let loads = 0;
const load = () => new Promise<unknown>((resolve) => setTimeout(() => resolve({ n: ++loads }), 5));
await Promise.all([refresh("runs", load), refresh("runs", load)]);
ok("two loads of one key at once run once", loads === 1 && (kept("runs")?.data as { n: number }).n === 1);

ok("updated now", updatedAgo(10_000, 40_000) === "updated now");
ok("updated N min ago", updatedAgo(0, 3 * 60_000 + 5_000) === "updated 3 min ago");
ok("updated N h ago", updatedAgo(0, 2 * 3_600_000) === "updated 2 h ago");

console.log(failed === 0 ? "\nALL PASS" : `\n${failed} FAILURES`);
if (failed > 0) throw new Error(`${failed} cache check failure(s)`);

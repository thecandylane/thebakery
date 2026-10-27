import { KeyedRefresher } from "./keyedRefresh.ts";

let failures = 0;
function assert(label: string, cond: boolean) {
  if (!cond) {
    console.error(`FAIL: ${label}`);
    failures++;
  } else {
    console.log(`pass: ${label}`);
  }
}

/** A manually-resolved promise, so a test can control exactly when a
 *  simulated fetch completes relative to other calls — the same technique
 *  `busyReconcile.check.ts` uses conceptually, adapted for async timing. */
function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

// 1. The exact reported bug: a second request that arrives while the first
//    is still in flight must NOT be dropped — it must cause a re-fetch once
//    the first completes, so a terminal event separated from an earlier one
//    by less than a round trip is never silently lost.
{
  const calls: string[] = [];
  const applied: string[] = [];
  const d1 = deferred<string>();
  const d2 = deferred<string>();
  const responses = [d1.promise, d2.promise];
  const refresher = new KeyedRefresher<string>(
    async (key) => {
      calls.push(key);
      return responses[calls.length - 1];
    },
    (key, value) => applied.push(`${key}=${value}`),
  );

  refresher.request("p1"); // starts fetch #1
  await flushMicrotasks();
  assert("first request starts exactly one fetch", calls.length === 1);

  refresher.request("p1"); // arrives while #1 is in flight — must not be dropped
  await flushMicrotasks();
  assert("a coalesced request does not start a second fetch immediately", calls.length === 1);

  d1.resolve("stale");
  await flushMicrotasks();
  assert("the first (now-stale) response is not applied", !applied.includes("p1=stale"));
  assert("the coalesced request triggers exactly one follow-up fetch", calls.length === 2);

  d2.resolve("fresh");
  await flushMicrotasks();
  assert("the follow-up fetch's result is applied", applied.includes("p1=fresh"));
  assert("nothing is applied twice", applied.filter((a) => a === "p1=fresh").length === 1);
}

// 2. No overlap: a request after the previous one has already resolved
//    starts a fresh fetch and applies normally.
{
  const calls: string[] = [];
  const applied: string[] = [];
  const refresher = new KeyedRefresher<string>(
    async (key) => {
      calls.push(key);
      return `v${calls.length}`;
    },
    (key, value) => applied.push(`${key}=${value}`),
  );
  refresher.request("p1");
  await flushMicrotasks();
  refresher.request("p1");
  await flushMicrotasks();
  assert("sequential (non-overlapping) requests each fetch", calls.length === 2);
  assert("the second result is applied", applied.includes("p1=v2"));
}

// 3. Independent keys never coalesce with each other.
{
  const calls: string[] = [];
  const refresher = new KeyedRefresher<string>(
    async (key) => {
      calls.push(key);
      return key;
    },
    () => {},
  );
  refresher.request("a");
  refresher.request("b");
  await flushMicrotasks();
  assert("requests for different keys both fetch", calls.includes("a") && calls.includes("b"));
}

// 4. A failed fetch does not wedge the coalescer: a later request still runs.
{
  let attempt = 0;
  const applied: string[] = [];
  const refresher = new KeyedRefresher<string>(
    async (key) => {
      attempt++;
      if (attempt === 1) throw new Error("network down");
      return "recovered";
    },
    (key, value) => applied.push(`${key}=${value}`),
  );
  refresher.request("p1");
  await flushMicrotasks();
  assert("a failed fetch applies nothing", applied.length === 0);
  refresher.request("p1");
  await flushMicrotasks();
  assert("a later request after a failure still fetches and applies", applied.includes("p1=recovered"));
}

if (failures > 0) throw new Error(`${failures} keyedRefresh check failure(s)`);

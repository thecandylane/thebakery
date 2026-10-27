// The generated client's calls as the node receives them: a path param
// encoded into the path, GET fields in the query (undefined and null left
// out), POST fields as a JSON body. fetch is stubbed; nothing leaves.
import { gitPull, jobOutput, stream } from "./api.generated.ts";

let failures = 0;
function assert(label: string, cond: boolean) {
  if (!cond) {
    console.error(`FAIL: ${label}`);
    failures++;
  } else {
    console.log(`pass: ${label}`);
  }
}

const calls: Array<{ url: string; init?: RequestInit }> = [];
globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
  calls.push({ url: String(url), init });
  return new Response("{}");
}) as typeof fetch;

await stream({ id: "wt:a/b c" });
assert("a path param is substituted, encoded", calls[0].url.endsWith("/stream/wt%3Aa%2Fb%20c"));
assert("a path param is not repeated in the query", !calls[0].url.includes("?"));
assert("a GET sends no body", calls[0].init?.method === "GET" && calls[0].init.body === undefined);

await jobOutput({ id: "j1", since: undefined });
assert("GET fields go in the query, undefined left out", calls[1].url.endsWith("/jobs/output?id=j1"));
await jobOutput({ id: "j1", since: null });
assert("null is left out of the query too", calls[2].url.endsWith("/jobs/output?id=j1"));
await jobOutput({ id: "j1", since: 0 });
assert("a zero is kept", calls[3].url.endsWith("/jobs/output?id=j1&since=0"));

await gitPull({ stream_id: "s1" });
const post = calls[4].init;
assert("a POST sends its fields as a JSON body", post?.method === "POST" && post.body === JSON.stringify({ stream_id: "s1" }));
assert("a POST says it is JSON", (post?.headers as Record<string, string>)["content-type"] === "application/json");

if (failures > 0) throw new Error(`${failures} apiClient check failure(s)`);

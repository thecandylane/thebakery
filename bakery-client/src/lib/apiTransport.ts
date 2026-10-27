// The transport under the generated client (api.generated.ts): node.ts's
// send(), so auth headers and the node's error messages stay in one place.
import type { Method } from "./api.generated.ts";
import { send } from "./node.ts";

/** GET puts the request's fields in the query string (skipping undefined and
 *  null, which a query cannot say); POST sends them as a JSON body. */
export function request<T>(method: Method, path: string, req: object, init?: RequestInit): Promise<T> {
  if (method === "GET") {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(req)) {
      if (value !== undefined && value !== null) query.set(key, String(value));
    }
    const qs = query.toString();
    return send<T>(qs ? `${path}?${qs}` : path, { ...init, method });
  }
  return send<T>(path, {
    ...init,
    method,
    headers: { "content-type": "application/json", ...((init?.headers as Record<string, string>) ?? {}) },
    body: JSON.stringify(req),
  });
}

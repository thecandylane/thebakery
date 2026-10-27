// Opaque route handles for stream ids.
//
// WHY: stream ids are `wt:{name}` today and repo-scoped tomorrow (HANDOFF §2
// debt #1). The URL bar is chrome — it must not teach humans `wt:…`, and no
// route may hard-code the id's shape. The workpane therefore carries an opaque
// handle and decodes it only to hand the id straight back to the node.
//
// base64url, not encodeURIComponent: percent-encoding still spells `wt%3A…`.
// This is a display/coupling boundary, not a security boundary — anyone can
// decode it, and nothing downstream trusts it.

/** Encode a stream id for use as a route parameter. */
export function toHandle(streamId: string): string {
  const bytes = new TextEncoder().encode(streamId);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** Decode a route parameter back into a stream id. Throws on garbage. */
export function fromHandle(handle: string): string {
  const b64 = handle.replace(/-/g, "+").replace(/_/g, "/");
  // atob is strict about length; restore the padding base64url dropped.
  const padded = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
  let bin: string;
  try {
    bin = atob(padded);
  } catch {
    throw new Error(`not a valid stream handle: ${handle}`);
  }
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

// The adapter pin parser on small fixtures — the same cases as the node's
// twin in crates/bakery-node/src/adapter_pin.rs — then on the real files.
import assert from "node:assert/strict";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { ADAPTER, exactVersion, lockedIntegrity, pinnedVersion, readAdapterPin } from "./adapter-pin.mjs";

for (const exact of ["0.84.0", "10.2.33", "1.0.0-beta.1", "1.0.0+build.7"]) {
  assert.ok(exactVersion(exact), exact);
}
for (const range of [
  "", "^0.84.0", "~0.84.0", ">=0.84.0", "0.84", "0.84.x", "1.x", "*", "latest",
  "1.0.0 - 2.0.0", "1.0.0 || 2.0.0", "workspace:*", "npm:other@1.0.0", "0.84.0; rm",
]) {
  assert.ok(!exactVersion(range), JSON.stringify(range));
}
console.log("ok   only an exact release is a pin");

const manifest = (spec) => JSON.stringify({ name: "x", dependencies: { [ADAPTER]: spec, react: "^19.3.0" } });
assert.equal(pinnedVersion(manifest("0.85.0"), ADAPTER), "0.85.0");
assert.equal(pinnedVersion(manifest("^0.85.0"), ADAPTER), null);
assert.equal(pinnedVersion('{"dependencies":{}}', ADAPTER), null);
assert.equal(pinnedVersion("not json", ADAPTER), null);
console.log("ok   the pin comes from the dependency entry and must be exact");

const lock = `lockfileVersion: '9.0'

patchedDependencies:
  '${ADAPTER}@0.84.0': b4d0acb4

packages:

  '${ADAPTER}@0.84.0':
    resolution: {integrity: sha512-old==}
    engines: {node: '>=22'}

  '${ADAPTER}@0.85.0':
    engines: {node: '>=22'}
    resolution: {integrity: sha512-new==, tarball: https://example.test/x.tgz}

  diff@9.0.0:
    resolution: {integrity: sha512-diff==}

snapshots:

  '${ADAPTER}@0.84.0(patch_hash=b4d0acb4)':
    dependencies:
      diff: 9.0.0
`;
assert.equal(lockedIntegrity(lock, ADAPTER, "0.84.0"), "sha512-old==");
assert.equal(lockedIntegrity(lock, ADAPTER, "0.85.0"), "sha512-new==");
assert.equal(lockedIntegrity(lock, "diff", "9.0.0"), "sha512-diff==");
assert.equal(lockedIntegrity(lock, ADAPTER, "0.86.0"), null);
const bare = "packages:\n\n  'p@1.0.0':\n    engines: {}\n\n  'q@1.0.0':\n    resolution: {integrity: sha512-q==}\n";
assert.equal(lockedIntegrity(bare, "p", "1.0.0"), null, "never borrows the next entry's integrity");
console.log("ok   the integrity is read from the packages entry only");

const pin = readAdapterPin(resolve(dirname(fileURLToPath(import.meta.url)), ".."));
assert.ok(exactVersion(pin.version) && pin.integrity.startsWith("sha512-"), JSON.stringify(pin));
console.log(`ok   bakery-client pins ${ADAPTER}@${pin.version}`);
console.log("adapter pin checks passed");

// Deploy's UI step (scripts/bakery.sh): installs a client build into the
// hash-named UI store and prints its hash, which bakery.sh writes into the
// build's ui-hash.
// Usage: node electron/install-ui.ts <build dir> <ui root>
import { installBundle } from "./bundle.ts";

const [build, root] = process.argv.slice(2);
if (!build || !root) {
  console.error("usage: node electron/install-ui.ts <build dir> <ui root>");
  process.exit(2);
}
console.log(installBundle(build, root));

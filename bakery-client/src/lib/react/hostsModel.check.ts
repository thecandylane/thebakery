import { covers, patternFor, sharedTenant } from "./hostsModel.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

ok("a bucket host is shared", sharedTenant("storage.googleapis.com") && sharedTenant("my-bucket.storage.googleapis.com"));
ok("a site under a shared domain is shared", sharedTenant("greenleaf.web.app") && sharedTenant("x.firebaseapp.com"));
ok("a project's own API host is not shared", !sharedTenant("firestore.googleapis.com") && !sharedTenant("pub.dev"));
ok("a pattern over a shared host is shared", sharedTenant("*.googleapis.com") && sharedTenant("*.web.app") && sharedTenant("*.app"));
ok("a pattern inside a shared domain is shared", sharedTenant("*.greenleaf.web.app"));
ok("a pattern over nothing shared is not", !sharedTenant("*.dartlang.org") && !sharedTenant("*.cocoapods.org"));

const g = patternFor(["firestore.googleapis.com", "identitytoolkit.googleapis.com", "storage.googleapis.com", "firebaseio.com"]);
ok("the domain most asks share", g?.pattern === "*.googleapis.com" && g.covers.length === 3, JSON.stringify(g));
ok("no pattern for one host per domain", patternFor(["pub.dev", "cdn.cocoapods.org"]) === null);
ok("a pattern never stands for the domain itself", patternFor(["googleapis.com", "x.googleapis.com"]) === null);
ok("a country's second level counts as one", patternFor(["a.example.co.uk", "b.example.co.uk"])?.pattern === "*.example.co.uk");
ok("an answer covers its own host, any case", covers("pub.dev", "PUB.dev") && !covers("pub.dev", "x.pub.dev"));
ok("a pattern covers what's under its domain, not the domain", covers("*.googleapis.com", "storage.googleapis.com") && !covers("*.googleapis.com", "googleapis.com"));

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} hosts model check failure(s)`);

// Stats shapes trimmed from real `container` 0.x and Docker (Colima) output
// on the operator's Mac, 2026-10-08. Lists come typed from the node.
import { itemsOf, portsText, statRows, type AppleCpuSample } from "./containersModel.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

ok(
  "ports: published, exposed only, udp",
  portsText([
    { host: 55322, container: 5432, protocol: "tcp" },
    { host: null, container: 8080, protocol: "tcp" },
    { host: 53, container: 53, protocol: "udp" },
  ]) === "55322→5432, 8080, 53→53/udp",
);

const ds = statRows("docker", itemsOf([{ ID: "fffd317c7915", Name: "supabase_db_demo", CPUPerc: "0.50%", MemUsage: "120MiB / 7.6GiB" }]), new Map(), 0);
ok("docker stats by name and id", ds.rows.get("supabase_db_demo")?.mem === "120MiB" && ds.rows.get("fffd317c7915")?.cpu === "0.50%");
const first = statRows("apple", [{ id: "c", cpuUsageUsec: 1_000_000, memoryUsageBytes: 2_000_000 }], new Map(), 1000);
ok("apple first sample has no cpu yet", first.rows.get("c")?.cpu === "" && first.rows.get("c")?.mem === "2.0 MB");
const second = statRows("apple", [{ id: "c", cpuUsageUsec: 1_500_000, memoryUsageBytes: 2_000_000 }], first.sample as AppleCpuSample, 2000);
ok("apple cpu from the delta", second.rows.get("c")?.cpu === "50.0%", second.rows.get("c")?.cpu);
ok("garbage is empty, not a throw", itemsOf("nope").length === 0);

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} containers model check failure(s)`);

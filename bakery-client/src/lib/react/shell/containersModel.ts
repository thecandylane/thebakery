// What the container manager still shapes itself. Lists come typed from the
// node (`GET /containers` → Listing); stats are still each backend's own
// JSON, and Apple reports CPU as cumulative time, so a percentage needs two
// samples. Pure and DOM-free: containersModel.check.ts runs it on node.

import type { Backend, Port } from "../../api.generated.ts";

export type StatRow = { cpu: string; mem: string };

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj => (v && typeof v === "object" && !Array.isArray(v) ? (v as Obj) : {});
const str = (v: unknown): string => (typeof v === "string" ? v : typeof v === "number" ? String(v) : "");
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);

/** The rows of a stats answer (a bare array, or `{ items }`). */
export function itemsOf(res: unknown): unknown[] {
  if (Array.isArray(res)) return res;
  const items = obj(res).items;
  return Array.isArray(items) ? items : [];
}

export function bytes(n: number | null): string {
  if (n === null) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** "55322→5432, 8080/udp": published host→container, or only exposed. */
export function portsText(ports: readonly Port[]): string {
  return ports
    .map((p) => `${p.host != null ? `${p.host}→` : ""}${p.container}${p.protocol && p.protocol !== "tcp" ? `/${p.protocol}` : ""}`)
    .join(", ");
}

/** Apple CPU samples by container id, with the time they were taken. */
export type AppleCpuSample = Map<string, { usec: number; at: number }>;

export function statRows(
  backend: Backend,
  items: unknown[],
  prev: AppleCpuSample,
  now: number,
): { rows: Map<string, StatRow>; sample: AppleCpuSample } {
  const rows = new Map<string, StatRow>();
  const sample: AppleCpuSample = new Map();
  for (const raw of items) {
    const o = obj(raw);
    if (backend === "docker") {
      const row = { cpu: str(o.CPUPerc), mem: str(o.MemUsage).split(" / ")[0] };
      // Docker stats name a container by short id and by name; rows hold the
      // full id, so match on either.
      rows.set(str(o.ID), row);
      rows.set(str(o.Name), row);
      continue;
    }
    const id = str(o.id);
    const usec = num(o.cpuUsageUsec);
    const before = prev.get(id);
    let cpu = "";
    if (usec !== null) {
      sample.set(id, { usec, at: now });
      if (before && now > before.at) cpu = `${(((usec - before.usec) / 1000 / (now - before.at)) * 100).toFixed(1)}%`;
    }
    rows.set(id, { cpu, mem: bytes(num(o.memoryUsageBytes)) });
  }
  return { rows, sample };
}

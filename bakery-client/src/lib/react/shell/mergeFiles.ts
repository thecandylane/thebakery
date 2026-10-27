// How many files a merge would change, from the node's merge preview (its
// summary, without the diff).

import type { MergePreview } from "$lib/api.generated";
import { parseUnifiedDiff } from "./unifiedDiff";

/** The files the merge would change on base, all of them: the list names
 *  only the first ones. An older node sends no count, or no list (it sent
 *  the diff), so they are counted from what it sent. */
export function mergedFiles(p: MergePreview): number {
  const total: number | undefined = p.files_total;
  const files: MergePreview["files"] | undefined = p.files;
  return total ?? (files ? files.length : parseUnifiedDiff(p.diff).length);
}

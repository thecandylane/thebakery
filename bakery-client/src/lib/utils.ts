// `cn` — the class-merging helper every vendored shadcn/AI Elements file
// imports as `@/lib/utils`. Kept at exactly that path so the upstream
// sources need no rewriting beyond the one recorded import patch.
import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

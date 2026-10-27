// Bakery's integration state for the source-owned AI Elements PromptInput.
//
// Upstream deliberately leaves provider attachment policy to the consumer.
// Bakery keeps the provider because the composer needs controlled text, so
// this hook supplies the missing pieces: the same type/size/count checks as
// local PromptInput state, plus process-lifetime draft files keyed to the
// bound stream + agent. Uploaded files still move into the node-owned
// attachment store; this map contains only unsent browser `File` objects.

import type { FileUIPart } from "ai";
import { nanoid } from "nanoid";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

export type PromptAttachmentError = {
  code: "max_files" | "max_file_size" | "accept";
  message: string;
};

export type PromptAttachmentConstraints = {
  accept?: string;
  multiple?: boolean;
  maxFiles?: number;
  maxFileSize?: number;
  onError?: (error: PromptAttachmentError) => void;
};

type CachedAttachment = { id: string; file: File };
type AttachmentEntry = CachedAttachment & { part: FileUIPart & { id: string } };

const drafts = new Map<string, readonly CachedAttachment[]>();
const draftListeners = new Map<string, Set<(next: readonly CachedAttachment[]) => void>>();

function publishDraft(key: string, next: readonly CachedAttachment[]): void {
  if (next.length === 0) drafts.delete(key);
  else drafts.set(key, next);
  for (const listener of draftListeners.get(key) ?? []) listener(next);
}

function matchesAccept(file: File, accept?: string): boolean {
  if (!accept || accept.trim() === "") return true;
  return accept
    .split(",")
    .map((pattern) => pattern.trim())
    .filter(Boolean)
    .some((pattern) => {
      if (pattern.startsWith(".")) return file.name.toLowerCase().endsWith(pattern.toLowerCase());
      if (pattern.endsWith("/*")) return file.type.startsWith(pattern.slice(0, -1));
      return file.type === pattern;
    });
}

function entry(file: File, id = nanoid()): AttachmentEntry {
  return {
    id,
    file,
    part: {
      id,
      type: "file",
      url: URL.createObjectURL(file),
      mediaType: file.type,
      filename: file.name,
    },
  };
}

export function usePromptAttachmentDrafts(draftKey?: string) {
  const [entries, setEntries] = useState<AttachmentEntry[]>(() =>
    draftKey ? (drafts.get(draftKey) ?? []).map(({ id, file }) => entry(file, id)) : [],
  );
  const entriesRef = useRef(entries);
  const mountedRef = useRef(true);
  const constraintsRef = useRef<PromptAttachmentConstraints>({});

  const commit = useCallback(
    (next: AttachmentEntry[]) => {
      if (draftKey) {
        const existing = new Set(entriesRef.current.map((candidate) => candidate.id));
        publishDraft(draftKey, next.map(({ id, file }) => ({ id, file })));
        // `next` may contain freshly-created temporary entries. Each mounted
        // subscriber materializes its own Blob URL from the canonical File;
        // retire only those temporary URLs after the synchronous publish.
        for (const candidate of next) {
          if (!existing.has(candidate.id) && candidate.part.url) URL.revokeObjectURL(candidate.part.url);
        }
        return;
      }
      entriesRef.current = next;
      if (mountedRef.current) setEntries(next);
    },
    [draftKey],
  );

  const add = useCallback(
    (files: File[] | FileList) => {
      const incoming = Array.from(files);
      if (incoming.length === 0) return;
      const constraints = constraintsRef.current;
      const accepted = incoming.filter((file) => matchesAccept(file, constraints.accept));
      if (accepted.length !== incoming.length) {
        constraints.onError?.({ code: "accept", message: "Some files do not match the accepted types." });
      }
      const sized = accepted.filter((file) =>
        constraints.maxFileSize ? file.size <= constraints.maxFileSize : true,
      );
      if (sized.length !== accepted.length) {
        constraints.onError?.({ code: "max_file_size", message: "Some files exceed the maximum size." });
      }
      const configuredMax = constraints.multiple === false ? 1 : constraints.maxFiles;
      const capacity =
        typeof configuredMax === "number"
          ? Math.max(0, configuredMax - entriesRef.current.length)
          : sized.length;
      const capped = sized.slice(0, capacity);
      if (capped.length !== sized.length) {
        constraints.onError?.({ code: "max_files", message: "Too many files. Some were not added." });
      }
      if (capped.length > 0) commit(entriesRef.current.concat(capped.map((file) => entry(file))));
    },
    [commit],
  );

  const remove = useCallback(
    (id: string) => {
      if (draftKey) {
        // Another projection may have mounted and added a newer attachment
        // while this hook's pane was closed. Consume from the shared
        // canonical draft by id, never from this instance's stale snapshot.
        publishDraft(draftKey, (drafts.get(draftKey) ?? []).filter((candidate) => candidate.id !== id));
        return;
      }
      const found = entriesRef.current.find((candidate) => candidate.id === id);
      if (found?.part.url) URL.revokeObjectURL(found.part.url);
      commit(entriesRef.current.filter((candidate) => candidate.id !== id));
    },
    [commit, draftKey],
  );

  const clear = useCallback(() => {
    for (const candidate of entriesRef.current) {
      if (candidate.part.url) URL.revokeObjectURL(candidate.part.url);
    }
    commit([]);
  }, [commit]);

  const configure = useCallback(
    (constraints: PromptAttachmentConstraints) => {
      constraintsRef.current = constraints;
      const previous = entriesRef.current;
      const configuredMax = constraints.multiple === false ? 1 : constraints.maxFiles;
      const typeValid = previous.filter((candidate) => matchesAccept(candidate.file, constraints.accept));
      const sizeValid = typeValid.filter((candidate) =>
        constraints.maxFileSize ? candidate.file.size <= constraints.maxFileSize : true,
      );
      const valid =
        typeof configuredMax === "number" ? sizeValid.slice(0, Math.max(0, configuredMax)) : sizeValid;
      if (valid.length === previous.length) return;

      const kept = new Set(valid.map((candidate) => candidate.id));
      for (const candidate of previous) {
        if (!kept.has(candidate.id) && candidate.part.url) URL.revokeObjectURL(candidate.part.url);
      }
      commit(valid);
      constraints.onError?.({
        code:
          typeValid.length !== previous.length
            ? "accept"
            : sizeValid.length !== typeValid.length
              ? "max_file_size"
              : "max_files",
        message: "Some draft attachments are not supported by the current session.",
      });
    },
    [commit],
  );

  useEffect(() => {
    mountedRef.current = true;
    if (draftKey) {
      const sync = (next: readonly CachedAttachment[]) => {
        const previous = entriesRef.current;
        const previousById = new Map(previous.map((candidate) => [candidate.id, candidate]));
        const nextEntries = next.map((candidate) => previousById.get(candidate.id) ?? entry(candidate.file, candidate.id));
        const kept = new Set(nextEntries.map((candidate) => candidate.id));
        for (const candidate of previous) {
          if (!kept.has(candidate.id) && candidate.part.url) URL.revokeObjectURL(candidate.part.url);
        }
        entriesRef.current = nextEntries;
        if (mountedRef.current) setEntries(nextEntries);
      };
      const current = draftListeners.get(draftKey) ?? new Set<(next: readonly CachedAttachment[]) => void>();
      current.add(sync);
      draftListeners.set(draftKey, current);
      // Close the render -> passive-effect window: an older, now-unmounted
      // submitter may have consumed an id after this hook took its useState
      // snapshot but before the listener became active.
      sync(drafts.get(draftKey) ?? []);
      return () => {
        mountedRef.current = false;
        current.delete(sync);
        if (current.size === 0) draftListeners.delete(draftKey);
        for (const candidate of entriesRef.current) {
          if (candidate.part.url) URL.revokeObjectURL(candidate.part.url);
        }
      };
    }
    return () => {
      mountedRef.current = false;
      for (const candidate of entriesRef.current) {
        if (candidate.part.url) URL.revokeObjectURL(candidate.part.url);
      }
    };
  }, [draftKey]);

  const files = useMemo(() => entries.map((candidate) => candidate.part), [entries]);
  return { files, add, remove, clear, configure };
}

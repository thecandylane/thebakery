// Tool calls, file diffs, and code, as the transcript shows them.
//
// A call is one quiet line — what it did, coloured by the kind of work, and
// what it touched — that opens onto its exact input and output. A command is
// the same quiet line, `$ command`: it never opens or folds by itself, its
// chevron opens the output, and the line itself brings up the Agent
// terminal. Colour only ever comes from theme.css: a row carries
// `data-tone`, ai-panel.css maps it to a `--tool-*` token, and code is
// coloured with shiki's `--shiki-*` variables (./highlight). Highlighting
// never changes a byte: every token's text is rendered in order, so what is
// selected and copied is exactly the text shown plain.

import { createContext, Fragment, memo, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  ArrowRightLeftIcon,
  BrainIcon,
  ChevronRightIcon,
  CroissantIcon,
  FileDiffIcon,
  FilePenLineIcon,
  FileTextIcon,
  GlobeIcon,
  SearchIcon,
  SquareTerminalIcon,
  Trash2Icon,
  WrenchIcon,
  type LucideIcon,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Loader } from "@/lib/ai-elements/loader";
import { collapse, countChanges, type DiffLine, diffLines, type DiffTag, splitRows } from "$lib/diff";
import type { CommandAt } from "$lib/terminalStream";
import { type CallStanding, callStanding, runExitCode, shellLine, stripAnsi, type TranscriptItem, withWholePayloads } from "$lib/transcript";
import type { AgentPanelActions } from "./contract";
import { languageForPath, tokenStyle, useHighlighted } from "./highlight";

type NativeTool = Extract<TranscriptItem, { kind: "tool" }>;
type AcpTool = Extract<TranscriptItem, { kind: "acp_tool" }>;

/** Every value shown verbatim: strings as-is, everything else as indented
 *  JSON. Nothing is summarized or clipped on this side. */
export function verbatim(v: unknown): string {
  if (typeof v === "string") return v;
  if (v === undefined) return "";
  return JSON.stringify(v, null, 2);
}

/** "json" when `text` is a JSON object or array, else undefined. */
function jsonLang(text: string): "json" | undefined {
  const t = text.trim();
  if (!/^[[{]/.test(t) || !/[\]}]$/.test(t)) return undefined;
  try {
    JSON.parse(t);
    return "json";
  } catch {
    return undefined;
  }
}

// ---- code -------------------------------------------------------------------

/** Exact text in a scrollable block, coloured when `lang` names a grammar.
 *  Until (or unless) the tokens exist it shows the plain text, so nothing
 *  ever waits on the highlighter. */
export const CodeView = memo(function CodeView({ code, lang, testId }: { code: string; lang?: string; testId?: string }) {
  const highlighted = useHighlighted(code, lang);
  return (
    <pre className="bakery-code" data-testid={testId}>
      <code>
        {highlighted
          ? highlighted.tokens.map((line, i) => (
              <Fragment key={i}>
                {i > 0 && "\n"}
                {line.map((token, j) => (
                  <span key={j} style={tokenStyle(token)}>
                    {token.content}
                  </span>
                ))}
              </Fragment>
            ))
          : code}
      </code>
    </pre>
  );
});

function Section({ label, tag, children }: { label: string; tag?: string; children: ReactNode }) {
  return (
    <div className="bakery-tool-section">
      <div className="bakery-tool-label">
        {label}
        {tag && <span className="bakery-tool-tag">{tag}</span>}
      </div>
      {children}
    </div>
  );
}

/** One labelled block of exact text; `tag` names what it reads as. */
export function CodeSection({
  label,
  text,
  lang,
  tag,
  testId,
}: {
  label: string;
  text: string;
  lang?: string;
  tag?: string;
  testId?: string;
}) {
  if (text === "") return null;
  return (
    <Section label={label} tag={tag ?? lang}>
      <CodeView code={text} lang={lang} testId={testId} />
    </Section>
  );
}

const DIFF_TAG: Record<DiffTag, string> = { "+": "add", "-": "del", "@": "gap", " ": "same" };

/** One side of a side-by-side row: a line, or hatching where that side has none. */
function SplitCell({ line, side }: { line: DiffLine | null; side: "old" | "new" }) {
  if (!line) return <div className="bakery-split-cell" data-tag="none" />;
  const tag = line.tag === " " ? "same" : side === "old" ? "del" : "add";
  return (
    <div className="bakery-split-cell" data-tag={tag}>
      <span aria-hidden className="bakery-split-mark" />
      <span className="bakery-split-text">{line.text}</span>
    </div>
  );
}

/** The folds a reader opened, by their place in `of`; a new diff starts
 *  folded again. */
function useOpenFolds(of: unknown): [ReadonlySet<number>, (at: number) => void] {
  const [opened, setOpened] = useState<{ of: unknown; at: ReadonlySet<number> }>({ of, at: new Set() });
  const at = opened.of === of ? opened.at : new Set<number>();
  return [at, (i) => setOpened({ of, at: new Set(at).add(i) })];
}

/** One file change side by side: removed lines on the left, added on the
 *  right, unchanged runs folded to a single marker that opens in place. */
export function SplitDiff({ oldText, newText, testId }: { oldText: string; newText: string; testId?: string }) {
  const rows = useMemo(() => splitRows(collapse(diffLines(oldText, newText))), [oldText, newText]);
  const [opened, open] = useOpenFolds(rows);
  return (
    <div className="bakery-split" data-testid={testId}>
      {rows.map((row, i) =>
        "gap" in row ? (
          opened.has(i) ? (
            row.hidden.map((line, j) => (
              <div className="bakery-split-row" key={`${i}:${j}`}>
                <SplitCell line={line} side="old" />
                <SplitCell line={line} side="new" />
              </div>
            ))
          ) : (
            <button className="bakery-split-gap w-full cursor-pointer text-left hover:underline" key={i} onClick={() => open(i)} type="button">
              {row.gap}
            </button>
          )
        ) : (
          <div className="bakery-split-row" key={i}>
            <SplitCell line={row.left} side="old" />
            <SplitCell line={row.right} side="new" />
          </div>
        ),
      )}
    </div>
  );
}

/** One file change as a compact unified diff: its path and counts on top,
 *  unchanged runs folded to a single "⋯ N unchanged lines" marker that
 *  opens in place. */
export function DiffView({
  path,
  oldText,
  newText,
  testId,
}: {
  path?: string;
  oldText: string;
  newText: string;
  testId?: string;
}) {
  const lines = useMemo(() => collapse(diffLines(oldText, newText)), [oldText, newText]);
  const { add, del } = useMemo(() => countChanges(lines), [lines]);
  const [opened, open] = useOpenFolds(lines);
  return (
    <div className="bakery-diff" data-testid={testId}>
      {path !== undefined && (
        <div className="bakery-diff-head">
          <FileDiffIcon aria-hidden className="size-3.5 shrink-0" />
          <span className="bakery-diff-path" title={path}>
            {path}
          </span>
          <span className="bakery-diff-count">
            <span data-tag="add">+{add}</span> <span data-tag="del">−{del}</span>
          </span>
        </div>
      )}
      <pre className="bakery-diff-body">
        {lines.map((line, i) =>
          line.tag === "@" && opened.has(i) ? (
            (line.hidden ?? []).map((same, j) => (
              <div className="bakery-diff-line" data-tag="same" key={`${i}:${j}`}>
                <span aria-hidden className="bakery-diff-sign" />
                <span>{same.text}</span>
              </div>
            ))
          ) : line.tag === "@" ? (
            <button className="bakery-diff-line w-full cursor-pointer text-left hover:underline" data-tag="gap" key={i} onClick={() => open(i)} type="button">
              <span aria-hidden className="bakery-diff-sign" />
              <span>{line.text}</span>
            </button>
          ) : (
            <div className="bakery-diff-line" data-tag={DIFF_TAG[line.tag]} key={i}>
              <span aria-hidden className="bakery-diff-sign">
                {line.tag === " " ? "" : line.tag}
              </span>
              <span>{line.text}</span>
            </div>
          ),
        )}
      </pre>
    </div>
  );
}

// ---- what a call is -----------------------------------------------------------

/** The kind of work a call does. Each is one `--tool-*` token in theme.css. */
export type Tone = "read" | "search" | "edit" | "exec" | "fetch" | "bakery" | "danger" | "other";

const TONE_ICON: Record<Tone, LucideIcon> = {
  read: FileTextIcon,
  search: SearchIcon,
  edit: FilePenLineIcon,
  exec: SquareTerminalIcon,
  fetch: GlobeIcon,
  bakery: CroissantIcon,
  danger: Trash2Icon,
  other: WrenchIcon,
};

/** Tool names as a claude-native runtime reports them. */
const NAMED_TONE: Record<string, Tone> = {
  Read: "read",
  LS: "read",
  NotebookRead: "read",
  Grep: "search",
  Glob: "search",
  ToolSearch: "search",
  Edit: "edit",
  MultiEdit: "edit",
  Write: "edit",
  NotebookEdit: "edit",
  Bash: "exec",
  BashOutput: "exec",
  KillShell: "exec",
  WebFetch: "fetch",
  WebSearch: "fetch",
};

/** ACP's `ToolKind`, for every other runtime: its verb, tone, and icon. */
const ACP_KIND: Record<string, [string, Tone, LucideIcon?]> = {
  read: ["Read", "read"],
  edit: ["Edit", "edit"],
  delete: ["Delete", "danger"],
  move: ["Move", "edit", ArrowRightLeftIcon],
  search: ["Search", "search"],
  execute: ["Run", "exec"],
  think: ["Think", "other", BrainIcon],
  fetch: ["Fetch", "fetch"],
};

/** The tone a permission request's ACP kind implies. */
export function toneForKind(kind: string | null): Tone {
  return ACP_KIND[kind ?? ""]?.[1] ?? "other";
}

/** Bakery's own HTTP-loop tools, by the first word `toolLabel` writes. */
const NATIVE_TONE: Record<string, Tone> = {
  read: "read",
  list: "read",
  glob: "search",
  grep: "search",
  write_file: "edit",
  delete_file: "danger",
  bash: "exec",
  exec: "exec",
  web_fetch: "fetch",
};

// The Baker's declared operations (baker_ops.rs), by family.
const BAKERY_OP = /^(project|socket|task|thread|memory|seat|gates|node|chat|conversation|runtime|turn)_[a-z_]+$/;

type Described = { verb: string; detail: string; tone: Tone; icon: LucideIcon };

function described(verb: string, detail: string, tone: Tone, icon?: LucideIcon): Described {
  return { verb, detail, tone, icon: icon ?? TONE_ICON[tone] };
}

const field = (o: Record<string, unknown>, key: string) => (typeof o[key] === "string" ? (o[key] as string) : "");

/** A deep absolute path keeps its last three segments on the one-line row;
 *  the expanded row and the hover title keep all of it. */
function shortPath(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return path.startsWith("/") && parts.length > 4 ? `…/${parts.slice(-3).join("/")}` : path;
}

function pathOf(input: unknown): string {
  if (!input || typeof input !== "object") return "";
  const o = input as Record<string, unknown>;
  return field(o, "file_path") || field(o, "notebook_path") || field(o, "path");
}

/** The command a call's input names: a string, or an argv (`command` for a
 *  runtime's own call, `argv` for Bakery's `run`). */
function commandOf(input: unknown): string | null {
  if (!input || typeof input !== "object") return null;
  const { command, argv } = input as Record<string, unknown>;
  if (typeof command === "string") return command;
  if (Array.isArray(command)) return shellLine(command);
  if (Array.isArray(argv)) return shellLine(argv);
  return null;
}

/** One line naming what a call's input touched, for the collapsed row. */
function inputSummary(input: unknown): string {
  if (typeof input === "string") return input;
  if (!input || typeof input !== "object" || Array.isArray(input)) return "";
  const o = input as Record<string, unknown>;
  const command = commandOf(o);
  if (command !== null) return command;
  const path = pathOf(o);
  const pattern = field(o, "pattern");
  if (pattern) return path ? `${pattern}  in ${shortPath(path)}` : pattern;
  const target = field(o, "url") || field(o, "query");
  if (target) return target;
  if (path) return shortPath(path);
  const description = field(o, "description");
  if (description) return description;
  return Object.entries(o)
    .filter(([, v]) => v !== undefined && v !== null)
    .map(([k, v]) =>
      typeof v === "object" ? `${k}: ${Array.isArray(v) ? `[${v.length}]` : "{…}"}` : `${k}: ${String(v)}`,
    )
    .join(" · ");
}

function describeAcp(item: AcpTool): Described {
  const name = item.title.trim() || item.toolCallId;
  const input = inputSummary(item.rawInput);
  const mcp = /^mcp__(.+?)__(.+)$/.exec(name);
  if (mcp) return described(mcp[2], input, mcp[1] === "bakery" ? "bakery" : "other");
  const named = NAMED_TONE[name];
  if (named) return described(name, input, named);
  const kind = ACP_KIND[item.toolKind ?? ""];
  // A bare identifier is the tool's own name…
  if (/^[\w.-]+$/.test(name)) {
    return described(name, input, BAKERY_OP.test(name) ? "bakery" : (kind?.[1] ?? "other"), kind?.[2]);
  }
  // …anything else is the runtime's sentence for the call ("Read src/x.ts",
  // "`ls -la`"), which already says what it touched.
  const [verb, tone, icon]: [string, Tone, LucideIcon?] = kind ?? ["Tool", "other"];
  let detail = name.replace(/^`+|`+$/g, "");
  if (detail.toLowerCase().startsWith(`${verb.toLowerCase()} `)) detail = detail.slice(verb.length + 1);
  return described(verb, detail, tone, icon);
}

function describeNative(item: NativeTool): Described {
  const space = item.label.indexOf(" ");
  const verb = space === -1 ? item.label : item.label.slice(0, space);
  let detail = space === -1 ? "" : item.label.slice(space + 1);
  // `toolLabel` writes an unrecognised tool's arguments as clipped JSON; the
  // item carries them whole.
  if (detail.startsWith("{")) {
    try {
      detail = inputSummary(JSON.parse(item.args));
    } catch {
      // keep the label's own text
    }
  }
  return described(verb, detail, NATIVE_TONE[verb] ?? (BAKERY_OP.test(verb) ? "bakery" : "other"));
}

// ---- the row ------------------------------------------------------------------

/** How a call stands (`callStanding`). */
export type Status = CallStanding;

const STATUS_TEXT: Record<Status, string> = {
  running: "running",
  done: "",
  failed: "failed",
  unfinished: "did not finish",
  "stopped-before": "stopped before it ran",
  "stopped-after": "stopped after it ran",
  stopped: "stopped",
};

const STATUS_TITLE: Partial<Record<Status, string>> = {
  unfinished: "Its turn ended before the runtime reported completed or failed.",
  "stopped-before": "Stop ended its turn before this call ran: nothing it would have done happened.",
  "stopped-after": "Stop ended its turn while this call ran: it had started, and what it got done is here.",
  stopped: "Stop ended its turn before the runtime said this call finished.",
};

/** A stopped command's line: how far it got, by its exit code or its last
 *  line of output, kept short. */
function stoppedAfter(exit: number | null, output: string): string {
  const last = output
    .split("\n")
    .map((l) => stripAnsi(l).trim())
    .filter(Boolean)
    .pop();
  const said = exit !== null ? `exit ${exit}` : last ? (last.length > 60 ? `${last.slice(0, 59)}…` : last) : "";
  return said ? `${STATUS_TEXT["stopped-after"]}: ${said}` : STATUS_TEXT["stopped-after"];
}

/** How a native call stands (`callStanding`). */
export const acpStatus = callStanding;

const firstLine = (text: string) => text.split("\n").find((l) => l.trim())?.trim() ?? "";
const basename = (path: string) => path.split("/").filter(Boolean).pop() ?? path;

/** A file change a call made: where, and the text before and after. */
type Change = { path: string; oldText: string; newText: string };

/** What an edit did, in words: "Added 4 lines", "Removed 2 lines", "Modified". */
function changeSummary(changes: readonly Change[]): string {
  let add = 0;
  let del = 0;
  for (const c of changes) {
    const n = countChanges(diffLines(c.oldText, c.newText));
    add += n.add;
    del += n.del;
  }
  if (add > 0 && del === 0) return `Added ${add} line${add === 1 ? "" : "s"}`;
  if (del > 0 && add === 0) return `Removed ${del} line${del === 1 ? "" : "s"}`;
  return add + del > 0 ? "Modified" : "";
}

/** Opens an edit's file in its thread's Code at the line it changed: the
 *  shell provides it; elsewhere file names stay plain text. `changeFor`
 *  names the recorded change behind an edit row's file, or null; `look`
 *  opens any file of the thread there, at its top. */
export type FileLinks = { open(changeHash: string): void; changeFor(path: string, at: string): string | null; look(path: string): void };
export const FileLinksContext = createContext<FileLinks | null>(null);

/** Brings the terminal panel forward on these terminals of the thread (the
 *  shell); with none named, a command's row: its Agent terminal, beside the
 *  thread, on that command (`at`). Nothing outside the shell. */
export const ShowTerminalsContext = createContext<((terminalIds: string[], at?: CommandAt) => void) | null>(null);

/** One call as a step: its name, a muted line under it, and then either its
 *  file changes side by side or, folded, one line of what went in and what
 *  came out. The row opens onto its exact input and output. */
function ToolLine({
  call,
  status,
  testId,
  at = "",
  about = "",
  io,
  changes,
  children,
}: {
  call: Described;
  status: Status;
  testId: string;
  /** When the call happened: its file links to the change recorded after it. */
  at?: string;
  /** What the call is for, in the agent's words. */
  about?: string;
  /** One line in and one line out, shown while folded. */
  io?: { in: string; out: string };
  /** The files it changed, shown side by side. */
  changes?: readonly Change[];
  children: ReactNode;
}) {
  const { verb, detail, tone } = call;
  const [open, setOpen] = useState(false);
  const edit = changes && changes.length > 0 ? changes : null;
  const summary = useMemo(() => (edit ? changeSummary(edit) : ""), [edit]);
  const links = useContext(FileLinksContext);
  const change = links && edit && at ? links.changeFor(edit[0].path, at) : null;
  const sub = edit
    ? status === "failed"
      ? `${verb} failed`
      : status === "done"
        ? summary
        : ""
    : io
      ? about !== io.in
        ? about
        : ""
      : detail;
  return (
    <Collapsible
      className="bakery-tool"
      data-status={status}
      data-testid={testId}
      data-tone={tone}
      onOpenChange={setOpen}
      open={open}
    >
      <CollapsibleTrigger className="bakery-tool-line" title={detail ? `${verb}  ${detail}` : verb}>
        <span className="bakery-tool-verb">{verb}</span>
        {edit &&
          (links && change ? (
            // Inside the row's toggle: a link that opens the file, not the row.
            <span
              className="bakery-tool-file"
              data-link=""
              onClick={(e) => {
                e.stopPropagation();
                links.open(change);
              }}
              onKeyDown={(e) => {
                if (e.key !== "Enter" && e.key !== " ") return;
                e.preventDefault();
                e.stopPropagation();
                links.open(change);
              }}
              role="link"
              tabIndex={0}
              title={`Open ${edit[0].path} in Code`}
            >
              {basename(edit[0].path)}
            </span>
          ) : (
            <span className="bakery-tool-file" title={edit[0].path}>
              {basename(edit[0].path)}
            </span>
          ))}
        {status === "running" && <Loader className="shrink-0" size={12} />}
        {STATUS_TEXT[status] && !(edit && status === "failed") && (
          <span className="bakery-tool-status" data-testid="tool-status" title={STATUS_TITLE[status]}>
            {STATUS_TEXT[status]}
          </span>
        )}
        <ChevronRightIcon aria-hidden className="bakery-tool-chevron size-3.5" />
      </CollapsibleTrigger>
      {sub && (
        <div className="bakery-tool-sub" data-failed={edit && status === "failed" ? "" : undefined}>
          {sub}
        </div>
      )}
      {edit?.map((c, i) => <SplitDiff key={`${c.path}:${i}`} newText={c.newText} oldText={c.oldText} testId="tool-change" />)}
      {!edit && io && !open && (io.in || io.out) && (
        <button className="bakery-tool-io" onClick={() => setOpen(true)} type="button">
          {io.in && (
            <span className="bakery-tool-io-row">
              <span className="bakery-tool-io-k" data-k="in">
                IN
              </span>
              <span className="bakery-tool-io-v">{io.in}</span>
            </span>
          )}
          {io.out && (
            <span className="bakery-tool-io-row">
              <span className="bakery-tool-io-k" data-k="out">
                OUT
              </span>
              <span className="bakery-tool-io-v">{io.out}</span>
            </span>
          )}
        </button>
      )}
      <CollapsibleContent>
        <div className="bakery-tool-body">{children}</div>
      </CollapsibleContent>
    </Collapsible>
  );
}

/** Output in a box capped in height: it scrolls, opens at its end, and keeps
 *  to the end as more arrives unless the reader has scrolled up. A terminal's
 *  bytes lose their escape codes, here, when the box is open. */
function OutputBox({ text, terminal = false, testId }: { text: string; terminal?: boolean; testId?: string }) {
  const shown = useMemo(() => (terminal ? stripAnsi(text) : text), [terminal, text]);
  const box = useRef<HTMLPreElement>(null);
  const atEnd = useRef(true);
  useLayoutEffect(() => {
    const el = box.current;
    if (el && atEnd.current) el.scrollTop = el.scrollHeight;
  }, [shown]);
  return (
    <pre
      className="bakery-code"
      data-testid={testId}
      onScroll={(e) => {
        const el = e.currentTarget;
        atEnd.current = el.scrollHeight - el.clientHeight - el.scrollTop < 2;
      }}
      ref={box}
    >
      <code>{shown}</code>
    </pre>
  );
}

/** A command as one line — `$ command`, a mark while it runs, how it ended —
 *  that stays folded: running or ending never opens or folds it, only its
 *  chevron does, onto the body. The line itself opens the thread's Agent
 *  terminal beside the thread, on this command's block, where it runs live. */
function CommandLine({
  command,
  call,
  status,
  exit,
  output = "",
  testId,
  children,
}: {
  command: string;
  /** Its call's id, the one the node marks its Agent terminal block with. */
  call?: string;
  status: Status;
  /** Its exit code, once it has one. */
  exit: number | null;
  /** What it wrote, for a stopped command's line. */
  output?: string;
  testId: string;
  children: ReactNode;
}) {
  const show = useContext(ShowTerminalsContext);
  // Running is the spinner's to say; once done, the exit code is. A command
  // Stop cut short says so, and how far it got.
  const end =
    status === "stopped-after"
      ? stoppedAfter(exit, output)
      : status === "stopped-before"
        ? STATUS_TEXT[status]
        : exit !== null
          ? `exit ${exit}`
          : status === "running"
            ? ""
            : STATUS_TEXT[status];
  const text = (
    <>
      <span aria-hidden className="bakery-command-prompt">
        $
      </span>
      <span className="bakery-command-text">{command}</span>
    </>
  );
  return (
    <Collapsible className="bakery-tool bakery-command" data-status={status} data-testid={testId} data-tone="exec">
      <div className="bakery-tool-line">
        {show ? (
          <button className="bakery-command-line" onClick={() => show([], { call, command })} title={command || undefined} type="button">
            {text}
          </button>
        ) : (
          <span className="bakery-command-line" title={command || undefined}>
            {text}
          </span>
        )}
        {status === "running" && <Loader className="shrink-0" size={12} />}
        {end && (
          <span className="bakery-tool-status" data-testid="tool-status" title={STATUS_TITLE[status]}>
            {end}
          </span>
        )}
        <CollapsibleTrigger aria-label="Output" className="bakery-command-expand">
          <ChevronRightIcon aria-hidden className="bakery-tool-chevron size-3.5" />
        </CollapsibleTrigger>
      </div>
      <CollapsibleContent>
        <div className="bakery-tool-body">{children}</div>
      </CollapsibleContent>
    </Collapsible>
  );
}

/** The change an `edit` call through Bakery's own loop asked for. */
function nativeChanges(item: NativeTool, verb: string): Change[] {
  if (verb !== "edit") return [];
  try {
    const args = JSON.parse(item.args) as Record<string, unknown>;
    return typeof args.old_string === "string" && typeof args.new_string === "string"
      ? [{ path: pathOf(args), oldText: args.old_string, newText: args.new_string }]
      : [];
  } catch {
    return [];
  }
}

/** A call through Bakery's own tool loop. */
export const NativeToolRow = memo(function NativeToolRow({ item }: { item: NativeTool }) {
  const call = describeNative(item);
  const changes = useMemo(() => nativeChanges(item, call.verb), [item, call.verb]);
  // One Stop caught before it ran: what was asked, and that nothing ran it.
  const status: Status = item.stopped === "before" ? "stopped-before" : item.ok ? "done" : "failed";
  // A command Bakery's own loop ran: one line, and what it recorded in a box.
  if (item.toolName === "run" || item.toolName === "bash") {
    return (
      <CommandLine call={item.callId} command={call.detail} exit={runExitCode(item.summary)} status={status} testId="tool-row">
        <OutputBox text={item.stopped ? "" : item.summary || (item.ok ? "" : "the tool call failed")} />
      </CommandLine>
    );
  }
  return (
    <ToolLine
      at={item.at}
      call={call}
      changes={changes}
      io={{ in: call.detail, out: item.stopped ? "" : firstLine(item.summary || (item.ok ? "" : "the tool call failed")) }}
      status={status}
      testId="tool-row"
    >
      <CodeSection label="Arguments" lang="json" text={item.args === "{}" ? "" : item.args} />
      {/* Bakery records a prose summary of what a call touched, not a
          payload — shown as the exact text, never highlighted. */}
      {!item.stopped && (item.summary || !item.ok) && (
        <div className="bakery-tool-section">
          <div className="bakery-tool-label">{item.ok ? "Result" : "Error"}</div>
          <p className="bakery-tool-summary" data-failed={item.ok ? undefined : ""}>
            {item.summary || "the tool call failed"}
          </p>
        </div>
      )}
    </ToolLine>
  );
});

/** A native (ACP) call's input, in the form that reads best: a command as
 *  shell, an edit as its diff, a write as the file's language, anything
 *  else as JSON. The exact input is also in Raw updates. */
function AcpInput({ item }: { item: AcpTool }) {
  const input = item.rawInput;
  if (input === undefined || input === null) return null;
  if (typeof input !== "object") return <CodeSection label="Input" testId="acp-tool-input" text={verbatim(input)} />;
  const o = input as Record<string, unknown>;
  const command = commandOf(o);
  if (command !== null) {
    const description = field(o, "description");
    return (
      <>
        {description && <p className="bakery-tool-note">{description}</p>}
        <CodeSection label="Command" lang="bash" testId="acp-tool-input" text={command} />
      </>
    );
  }
  const path = pathOf(o);
  // A runtime that also reports the change as a diff block shows it once.
  const reportsDiff = item.content.some((b) => (b as { type?: unknown } | null)?.type === "diff");
  if (!reportsDiff && typeof o.old_string === "string" && typeof o.new_string === "string") {
    return <DiffView newText={o.new_string} oldText={o.old_string} path={path || "edit"} testId="acp-tool-input" />;
  }
  if (!reportsDiff && typeof o.content === "string" && path) {
    return <CodeSection label="Content" lang={languageForPath(path)} testId="acp-tool-input" text={o.content} />;
  }
  if (Object.keys(o).length === 0) return null;
  return <CodeSection label="Input" lang="json" testId="acp-tool-input" text={verbatim(o)} />;
}

/** One ACP content block, as the runtime reported it. */
function AcpContentBlock({ block, index }: { block: unknown; index: number }) {
  const b = (block && typeof block === "object" ? block : {}) as Record<string, unknown>;
  if (b.type === "diff") {
    return (
      <DiffView
        newText={verbatim(b.newText ?? "")}
        oldText={verbatim(b.oldText ?? "")}
        path={typeof b.path === "string" ? b.path : `diff ${index + 1}`}
      />
    );
  }
  if (b.type === "content") {
    const inner = (b.content && typeof b.content === "object" ? b.content : {}) as Record<string, unknown>;
    if (typeof inner.text === "string") return <CodeSection label="Content" lang={jsonLang(inner.text)} text={inner.text} />;
    return <CodeSection label="Content" lang="json" text={verbatim(inner)} />;
  }
  if (b.type === "terminal") return <CodeSection label="Terminal" text={verbatim(b.terminalId ?? b)} />;
  return <CodeSection label="Content" lang="json" text={verbatim(block)} />;
}

function isTextBlocks(v: unknown): v is { type: "text"; text: string }[] {
  return (
    Array.isArray(v) &&
    v.length > 0 &&
    v.every((b) => !!b && typeof b === "object" && (b as { type?: unknown }).type === "text" && typeof (b as { text?: unknown }).text === "string")
  );
}

/** A call's result as text, and the grammar it reads as. JSON a runtime
 *  sent as one compact string is re-indented for reading and tagged as
 *  formatted; the exact bytes stay in Raw updates. A file read is coloured
 *  as the file's language unless the runtime numbered its lines. */
function outputView(value: unknown, tone: Tone, path: string): { text: string; lang?: string; tag?: string } {
  if (value === undefined) return { text: "" };
  let out: string;
  if (typeof value === "string") out = value;
  else if (isTextBlocks(value)) out = value.map((b) => b.text).join("\n");
  else return { text: verbatim(value), lang: "json" };
  if (jsonLang(out)) {
    const pretty = JSON.stringify(JSON.parse(out.trim()), null, 2);
    return pretty === out ? { text: out, lang: "json" } : { text: pretty, lang: "json", tag: "json · formatted" };
  }
  if (tone === "read" && path && !/^\s*\d+[→\t]/.test(out)) {
    const lang = languageForPath(path);
    if (lang) return { text: out, lang };
  }
  return { text: out };
}

function locationLines(locations: unknown[]): string {
  return locations
    .map((l) => {
      const o = (l && typeof l === "object" ? l : {}) as Record<string, unknown>;
      return typeof o.path === "string" ? `${o.path}${typeof o.line === "number" ? `:${o.line}` : ""}` : verbatim(l);
    })
    .join("\n");
}

/** A native (ACP) tool call, every update folded into one row. */
/** Reads the whole of each cut update, once the row is open (its body
 *  mounts only then), skipping what is already read. */
function ReadWhole({
  refs,
  have,
  actions,
  onRead,
  onError,
}: {
  refs: readonly { sha: string }[];
  have: ReadonlyMap<string, unknown>;
  actions: AgentPanelActions;
  onRead: (sha: string, value: unknown) => void;
  onError: (error: string) => void;
}) {
  // Keyed by every ref, not by what is left: reading one must not restart
  // the run and read the next twice.
  const key = refs.map((r) => r.sha).join(",");
  const held = useRef(have);
  held.current = have;
  useEffect(() => {
    let live = true;
    void (async () => {
      for (const sha of key ? key.split(",") : []) {
        if (held.current.has(sha)) continue;
        const got = await actions.fetchToolPayload(sha);
        if (!live) return;
        if (got.status === "error") return onError(got.error);
        if (got.status === "ok") onRead(sha, got.value);
      }
    })();
    return () => {
      live = false;
    };
  }, [actions, key]);
  return null;
}

export const AcpToolRow = memo(function AcpToolRow({ item: cut, actions }: { item: AcpTool; actions: AgentPanelActions }) {
  // What the Heart's copy cut is read whole once the row opens, and shown
  // in place: input, output, content and a command's terminal output.
  const [whole, setWhole] = useState<ReadonlyMap<string, unknown>>(new Map());
  const [error, setError] = useState("");
  const item = useMemo(() => (whole.size ? withWholePayloads(cut, whole) : cut), [cut, whole]);

  // The runtime's own word that it finished first; then what Stop did to it;
  // else a failure or a command's exit code; "did not finish" only for a
  // call whose turn ended with none of them.
  const exit = item.terminal?.exitCode ?? null;
  const status = acpStatus(item);
  const call = describeAcp(item);
  const output = outputView(item.rawOutput, call.tone, pathOf(item.rawInput));
  const input =
    item.rawInput && typeof item.rawInput === "object" && !Array.isArray(item.rawInput)
      ? (item.rawInput as Record<string, unknown>)
      : {};
  // The file changes it reports (diff blocks), else the one its input asks for.
  const reported = item.content.flatMap((block): Change[] => {
    const b = (block && typeof block === "object" ? block : {}) as Record<string, unknown>;
    return b.type === "diff"
      ? [{ path: typeof b.path === "string" ? b.path : "", oldText: verbatim(b.oldText ?? ""), newText: verbatim(b.newText ?? "") }]
      : [];
  });
  const asked =
    reported.length === 0 && typeof input.old_string === "string" && typeof input.new_string === "string"
      ? [{ path: pathOf(input), oldText: input.old_string, newText: input.new_string }]
      : [];
  const changes = reported.length > 0 ? reported : asked;
  // A command's terminal replaces its input and the bare terminal marker
  // among its content blocks; changes shown side by side are not repeated.
  const content = item.content.filter((b) => {
    const type = (b as { type?: unknown } | null)?.type;
    return !(item.terminal && type === "terminal") && !(reported.length > 0 && type === "diff");
  });
  const rest = (
    <>
      {content.map((block, i) => (
        <AcpContentBlock block={block} index={i} key={i} />
      ))}
      <CodeSection label="Output" lang={output.lang} tag={output.tag} testId="acp-tool-output" text={output.text} />
      {item.locations.length > 0 && <CodeSection label="Locations" text={locationLines(item.locations)} />}
      {cut.fullRefs.length > 0 && (
        <ReadWhole
          actions={actions}
          have={whole}
          onError={setError}
          onRead={(sha, value) => setWhole((w) => new Map(w).set(sha, value))}
          refs={cut.fullRefs}
        />
      )}
      {error && <div className="text-destructive text-xs">{error}</div>}
      <details className="bakery-tool-raw">
        <summary>
          Raw updates ({item.updates.length}) · {item.toolCallId}
        </summary>
        <div className="bakery-tool-raw-list">
          {item.updates.map((u, i) => (
            <CodeSection
              key={i}
              label={`${u.event} · ${u.at}`}
              lang={typeof u.detail === "string" ? undefined : "json"}
              text={verbatim(u.detail)}
            />
          ))}
        </div>
      </details>
    </>
  );
  // A command the runtime ran in its own terminal: one line, and its output
  // as plain text in a box (the exact bytes stay in Raw updates).
  if (cut.terminal) {
    // Before its input arrives a command's title is only "Terminal".
    const command = commandOf(input) ?? (item.title === "Terminal" ? "" : item.title);
    const description = field(input, "description");
    return (
      <CommandLine call={item.toolCallId} command={command} exit={exit} output={(item.terminal ?? cut.terminal).output} status={status} testId="acp-tool-row">
        {description && <p className="bakery-tool-note">{description}</p>}
        <OutputBox terminal testId="acp-terminal" text={(item.terminal ?? cut.terminal).output} />
        {rest}
      </CommandLine>
    );
  }
  return (
    <ToolLine
      about={field(input, "description")}
      at={item.at}
      call={call}
      changes={changes}
      io={{ in: inputSummary(item.rawInput) || call.detail, out: firstLine(output.text) }}
      status={status}
      testId="acp-tool-row"
    >
      {asked.length === 0 && <AcpInput item={item} />}
      {rest}
    </ToolLine>
  );
});

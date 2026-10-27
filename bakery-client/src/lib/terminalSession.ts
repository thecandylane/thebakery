// The studio's terminals as sessions that live as long as the window: one
// per node terminal, holding its xterm, its socket and everything it
// printed. A view (Terminal.svelte) only lends one a place on screen, so
// switching tabs, places or workspaces, hiding the bar and going to the
// Floor never rebuild or replay it. A session reconnects on its own after a
// drop and catches up from the byte where it left off (the node's word,
// terminalStream.ts), and a stretch the node no longer keeps is said where
// it was. It draws with WebGL while it shows (the DOM renderer otherwise,
// and whenever WebGL fails), measures the mono font once it has loaded, and
// takes a new size only once a drag settles.

import { Terminal as XTerm, type IDecoration, type IMarker, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { node } from "$lib/node";
import { THEME_CHANGED_EVENT } from "$lib/theme";
import { cssColorToHex } from "$lib/themeColors";
import { type CommandAt, droppedLine, exitOf, headerOf, isGone, retryDelay, streamWord } from "$lib/terminalStream";

export type { CommandAt };
export type SessionStatus = { live: boolean; text: string };

/** The OSC each Agent terminal block opens with (the node's `CALL_MARK_OSC`). */
const CALL_MARK_OSC = 7766;
/** How long a size must hold before the terminal and its process take it. */
const SETTLE_MS = 120;
/** Sessions kept with no view, the least recently shown going first. */
const KEPT_UNSEEN = 24;
/** The longest a terminal waits for the mono font before it draws anyway. */
const FONT_WAIT_MS = 1500;
const FONT_SIZE = 12;

const sessions = new Map<string, TerminalSession>();
const keyOf = (streamId: string, terminalId: string) => `${streamId} ${terminalId}`;
/** No WebGL2 here: every terminal draws with the DOM renderer. */
let noGpu = false;

function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** The mono font as the app styles it, by its families: a canvas can't
 *  resolve a CSS variable, so measuring `var(--font-mono)` measured another
 *  font. */
function monoStack(): string {
  return cssVar("--font-mono") || "ui-monospace, Menlo, monospace";
}

const firstFamily = () => monoStack().split(",")[0].trim().replace(/^["']|["']$/g, "");

let monoReady: Promise<void> | null = null;
/** Once the mono font has loaded, or FONT_WAIT_MS on, whichever is first. */
function monoLoaded(): Promise<void> {
  monoReady ??= Promise.race([
    document.fonts.load(`${FONT_SIZE}px ${monoStack()}`).then(
      () => undefined,
      () => undefined,
    ),
    new Promise<void>((done) => setTimeout(done, FONT_WAIT_MS)),
  ]);
  return monoReady;
}

/** theme.css is the only colour origin (07d-A2 §1): xterm needs literal
 *  colours, so it reads the same custom properties Monaco reads. */
function xtermTheme(): ITheme {
  const v = (name: string) => cssColorToHex(cssVar(name));
  return {
    background: v("--bg-rail"),
    foreground: v("--fg-body"),
    cursor: v("--accent"),
    cursorAccent: v("--bg-rail"),
    selectionBackground: v("--accent-border"),
    // ANSI output follows the operator's shared semantic colours too.
    black: v("--bg-rail"),
    red: v("--bad"),
    green: v("--ok"),
    yellow: v("--warn"),
    blue: v("--accent"),
    magenta: v("--violet"),
    cyan: v("--syntax-type"),
    white: v("--fg-body"),
    brightBlack: v("--fg-faint"),
    brightRed: v("--bad"),
    brightGreen: v("--ok"),
    brightYellow: v("--warn"),
    brightBlue: v("--accent-fg"),
    brightMagenta: v("--syntax-keyword"),
    brightCyan: v("--syntax-string"),
    brightWhite: v("--fg"),
  };
}

/** A block's header, lit: the violet a fifth of the way into the terminal's own background. */
function litColor(): string {
  const rgb = (hex: string) => [1, 3, 5].map((i) => Number.parseInt(hex.slice(i, i + 2), 16));
  const [bg, fg] = [rgb(cssColorToHex(cssVar("--bg-rail"))), rgb(cssColorToHex(cssVar("--violet")))];
  if ([...bg, ...fg].some(Number.isNaN)) return "#3a3150";
  return `#${bg.map((b, i) => Math.round(b + (fg[i] - b) * 0.22).toString(16).padStart(2, "0")).join("")}`;
}

let listening = false;
/** What every session follows: the scheme, the font, and the network and
 *  the window coming back (a drop's reconnect goes at once then). */
function listen() {
  if (listening) return;
  listening = true;
  window.addEventListener(THEME_CHANGED_EVENT, () => {
    const theme = xtermTheme();
    for (const s of sessions.values()) s.retheme(theme);
  });
  document.fonts.addEventListener("loadingdone", (e) => {
    const family = firstFamily();
    if (!(e as FontFaceSetLoadEvent).fontfaces.some((f) => f.family.replace(/["']/g, "") === family)) return;
    for (const s of sessions.values()) s.remeasure();
  });
  const wake = () => {
    for (const s of sessions.values()) s.wake();
  };
  window.addEventListener("online", wake);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") wake();
  });
}

/** The window's session for a node terminal, made the first time it's asked for. */
export function session(streamId: string, terminalId: string, readOnly = false): TerminalSession {
  listen();
  const key = keyOf(streamId, terminalId);
  let s = sessions.get(key);
  if (!s) {
    s = new TerminalSession(streamId, terminalId, readOnly);
    sessions.set(key, s);
  }
  return s;
}

/** Lets a session go: its socket closes, the node's terminal runs on. */
export function dropSession(streamId: string, terminalId: string) {
  sessions.get(keyOf(streamId, terminalId))?.dispose();
}

/** Shows a command's block in a stream's Agent terminal, once it has it. */
export function revealCall(streamId: string, terminalId: string, at: CommandAt) {
  session(streamId, terminalId, true).reveal(at);
}

/** At most KEPT_UNSEEN sessions wait with no view; the longest unseen go. */
function trimUnseen() {
  const unseen = [...sessions.values()].filter((s) => !s.shown).sort((a, b) => a.lastShown - b.lastShown);
  for (const s of unseen.slice(0, Math.max(0, unseen.length - KEPT_UNSEEN))) s.dispose();
}

export class TerminalSession {
  readonly term: XTerm;
  private readonly fit = new FitAddon();
  private gpu: WebglAddon | null = null;
  /** xterm's own element, lent to whichever view shows the session. */
  private readonly el = document.createElement("div");
  private opened = false;
  private view: HTMLElement | null = null;
  lastShown = 0;
  private resizes: ResizeObserver | null = null;
  private settle = 0;
  private ws: WebSocket | null = null;
  /** Bumped on every connect, so a stale attempt never touches the live one. */
  private gen = 0;
  /** Where the next byte the node sends sits in the terminal's whole output. */
  private offset = 0;
  /** The first output after a (re)connect is the replay: a wanted block may be in it. */
  private replayed = false;
  private retries = 0;
  private retryTimer = 0;
  /** Exited, gone from the node, or let go: it never reconnects. */
  private over = false;
  private closing = false;
  private disposed = false;
  private state: SessionStatus = { live: false, text: "connecting…" };
  private readonly statusFns = new Set<(s: SessionStatus) => void>();
  private readonly exitFns = new Set<(code: number | null) => void>();
  /** An Agent terminal's blocks: where each call's first header starts. */
  private readonly marks = new Map<string, IMarker>();
  private wanted: CommandAt | null = null;
  private lit: IDecoration | null = null;
  private fontSwap = false;

  constructor(
    readonly streamId: string,
    readonly terminalId: string,
    readonly readOnly: boolean,
  ) {
    this.el.style.height = "100%";
    this.term = new XTerm({
      // Decorations (a revealed block's header) are xterm's proposed API.
      allowProposedApi: true,
      cursorBlink: !readOnly,
      disableStdin: readOnly,
      fontFamily: monoStack(),
      fontSize: FONT_SIZE,
      scrollback: readOnly ? 20_000 : 5_000,
      theme: xtermTheme(),
    });
    this.term.loadAddon(this.fit);
    this.term.onData((data) => {
      if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(new TextEncoder().encode(data));
    });
    this.term.onResize(({ cols, rows }) => this.sendSize(cols, rows));
    if (readOnly) this.term.parser.registerOscHandler(CALL_MARK_OSC, (call) => this.markCall(call));
    void this.connect();
  }

  get status(): SessionStatus {
    return this.state;
  }

  get shown(): boolean {
    return this.view !== null;
  }

  /** Told now and on every change. */
  onStatus(fn: (s: SessionStatus) => void): () => void {
    this.statusFns.add(fn);
    fn(this.state);
    return () => this.statusFns.delete(fn);
  }

  /** Told once its process exits on its own: `null` when the code is unknown. */
  onExit(fn: (code: number | null) => void): () => void {
    this.exitFns.add(fn);
    return () => this.exitFns.delete(fn);
  }

  /** Its view's kill is under way: the socket's close is not a drop to reconnect. */
  setClosing(on: boolean) {
    this.closing = on;
  }

  /** Shows the session in `view`: drawn there with WebGL, sized to it. */
  attach(view: HTMLElement) {
    if (this.disposed) return;
    this.view = view;
    this.lastShown = Date.now();
    if (this.el.parentElement !== view) view.appendChild(this.el);
    this.resizes?.disconnect();
    this.resizes = new ResizeObserver(() => this.settleSoon());
    this.resizes.observe(view);
    void monoLoaded().then(() => this.show(view));
  }

  /** Takes the session off `view`; it stays live, out of sight. */
  detach(view: HTMLElement) {
    if (this.view !== view) return;
    this.view = null;
    this.lastShown = Date.now();
    window.clearTimeout(this.settle);
    this.resizes?.disconnect();
    this.resizes = null;
    this.dropGpu();
    this.el.remove();
    if (this.over) this.dispose();
    else trimUnseen();
  }

  /** Scrolls an Agent terminal to a command's block and lights its header,
   *  now or as soon as the block arrives. */
  reveal(at: CommandAt) {
    this.wanted = at;
    this.revealWanted();
  }

  retheme(theme: ITheme) {
    this.term.options.theme = theme;
  }

  /** The mono font loaded after the terminal measured: measure again. xterm
   *  measures when its font option changes, so the same families are told
   *  apart by a space. */
  remeasure() {
    if (!this.opened) return;
    this.fontSwap = !this.fontSwap;
    this.term.options.fontFamily = monoStack() + (this.fontSwap ? " " : "");
    this.fitNow();
  }

  /** The network or the window came back: a waiting reconnect goes now. */
  wake() {
    if (this.over || !this.retryTimer) return;
    window.clearTimeout(this.retryTimer);
    this.retryTimer = 0;
    this.retries = 0;
    void this.connect();
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.over = true;
    this.gen++;
    window.clearTimeout(this.retryTimer);
    window.clearTimeout(this.settle);
    this.closeSocket();
    this.resizes?.disconnect();
    this.dropGpu();
    this.lit?.dispose();
    this.term.dispose();
    this.el.remove();
    this.statusFns.clear();
    this.exitFns.clear();
    const key = keyOf(this.streamId, this.terminalId);
    if (sessions.get(key) === this) sessions.delete(key);
  }

  private setState(live: boolean, text: string) {
    this.state = { live, text };
    for (const fn of this.statusFns) fn(this.state);
  }

  private sendSize(cols: number, rows: number) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify({ resize: { cols, rows } }));
  }

  private closeSocket() {
    const ws = this.ws;
    if (!ws) return;
    this.ws = null;
    ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
    ws.close();
  }

  /** Attaches from where it left off. The ticket comes first, over a plain
   *  POST: a WebSocket refusal never says why, a ticket refusal does. */
  private async connect() {
    if (this.over) return;
    const gen = ++this.gen;
    this.closeSocket();
    this.replayed = false;
    if (this.retries > 0 || this.offset > 0) this.setState(false, "reconnecting…");
    let ticket: string;
    try {
      ticket = (await node.terminalTicket(this.streamId, this.terminalId)).ticket;
    } catch (e) {
      if (gen !== this.gen) return;
      if (isGone(e instanceof Error ? e.message : String(e))) return this.end("ended", undefined);
      return this.retryLater();
    }
    if (gen !== this.gen || this.over) return;
    const ws = new WebSocket(node.terminalWsUrl(this.streamId, this.terminalId, ticket, this.offset, true));
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      if (gen !== this.gen) return;
      this.retries = 0;
      this.setState(true, "live");
      this.sendSize(this.term.cols, this.term.rows);
    };
    ws.onmessage = (ev) => {
      if (gen !== this.gen) return;
      if (typeof ev.data === "string") {
        const word = streamWord(ev.data);
        if (!word) return void this.term.write(ev.data);
        if (word.dropped > 0) this.term.write(droppedLine(word.dropped));
        this.offset = word.at;
        return;
      }
      const bytes = new Uint8Array(ev.data as ArrayBuffer);
      this.offset += bytes.byteLength;
      const replay = !this.replayed;
      this.replayed = true;
      this.term.write(bytes, replay ? () => this.revealWanted() : undefined);
    };
    ws.onclose = (ev) => {
      if (gen !== this.gen) return;
      this.ws = null;
      if (this.closing) return this.end("stopped", undefined);
      const code = exitOf(ev.reason ?? "");
      if (code !== undefined) return this.end(ev.reason, code);
      this.retryLater();
    };
    this.ws = ws;
  }

  /** A dropped connection: try again, soon at first, then every 8 seconds. */
  private retryLater() {
    if (this.over) return;
    this.setState(false, "reconnecting…");
    window.clearTimeout(this.retryTimer);
    this.retryTimer = window.setTimeout(() => {
      this.retryTimer = 0;
      void this.connect();
    }, retryDelay(this.retries++));
  }

  private end(text: string, code: number | null | undefined) {
    this.over = true;
    window.clearTimeout(this.retryTimer);
    this.retryTimer = 0;
    this.setState(false, text);
    if (code !== undefined) for (const fn of this.exitFns) fn(code);
  }

  private show(view: HTMLElement) {
    if (this.disposed || this.view !== view) return;
    if (!this.opened) {
      this.term.open(this.el);
      this.opened = true;
    }
    this.useGpu();
    this.fitNow();
    this.term.refresh(0, this.term.rows - 1);
    this.revealWanted();
  }

  private useGpu() {
    if (this.gpu || noGpu || !this.opened) return;
    try {
      const gpu = new WebglAddon();
      gpu.onContextLoss(() => this.dropGpu());
      this.term.loadAddon(gpu);
      this.gpu = gpu;
    } catch {
      // No WebGL2: the DOM renderer draws every terminal.
      noGpu = true;
    }
  }

  private dropGpu() {
    const gpu = this.gpu;
    this.gpu = null;
    gpu?.dispose();
  }

  private settleSoon() {
    window.clearTimeout(this.settle);
    this.settle = window.setTimeout(() => this.fitNow(), SETTLE_MS);
  }

  private fitNow() {
    const view = this.view;
    if (!this.opened || !view || view.clientWidth === 0 || view.clientHeight === 0) return;
    try {
      this.fit.fit();
    } catch {
      // Not laid out yet: the next settle fits it.
    }
  }

  private markCall(call: string): boolean {
    if (!this.marks.has(call)) {
      const marker = this.term.registerMarker(0);
      this.marks.set(call, marker);
      marker.onDispose(() => {
        if (this.marks.get(call) === marker) this.marks.delete(call);
      });
      if (this.wanted?.call === call) queueMicrotask(() => this.revealWanted());
    }
    return true;
  }

  private revealWanted() {
    const at = this.wanted;
    if (!at || !this.opened) return;
    const line = this.lineOf(at);
    if (line === null) return;
    this.wanted = null;
    this.term.scrollToLine(Math.max(0, line - 1));
    this.light(line);
  }

  /** Where a command's block starts: by its call's mark, else its header's
   *  last showing (a block marked under another id). */
  private lineOf(at: CommandAt): number | null {
    const marked = at.call ? this.marks.get(at.call) : undefined;
    if (marked && !marked.isDisposed) return marked.line;
    const header = headerOf(at.command);
    const want = header.length > this.term.cols ? header.slice(0, this.term.cols) : header;
    const buf = this.term.buffer.active;
    for (let y = buf.length - 1; y >= 0; y--) {
      if (buf.getLine(y)?.translateToString(true) === want) return y;
    }
    return null;
  }

  private light(line: number) {
    this.lit?.dispose();
    const buf = this.term.buffer.active;
    const marker = this.term.registerMarker(line - (buf.baseY + buf.cursorY));
    this.lit = this.term.registerDecoration({ marker, width: this.term.cols, backgroundColor: litColor(), layer: "bottom" }) ?? null;
    if (this.lit) this.lit.onDispose(() => marker.dispose());
    else marker.dispose();
  }
}

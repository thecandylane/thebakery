// Mac notifications with buttons, for asks the operator answers from
// anywhere (main.ts makes the native ones). Plain node: shell.check.ts runs
// it against stand-ins.

export type NoticeSpec = { id: string; title: string; body: string; actions: string[] };

/** What main.ts needs of Electron's Notification (each event's arguments
 *  are Electron's own). */
export type Native = {
  on(event: "action" | "click" | "close" | "failed", listener: (...args: any[]) => void): unknown;
  show(): void;
  close(): void;
};

const text = (value: unknown, most: number) => (typeof value === "string" ? value.slice(0, most) : "");

/** The page's spec, checked: an id, a title, and one to four buttons. */
export function noticeSpec(raw: unknown): NoticeSpec {
  const spec = (raw ?? {}) as Record<string, unknown>;
  const id = text(spec.id, 200);
  const actions = Array.isArray(spec.actions) ? spec.actions.map((a) => text(a, 60)).filter(Boolean) : [];
  if (!id || !text(spec.title, 1)) throw new Error("a notification needs an id and a title");
  if (actions.length < 1 || actions.length > 4) throw new Error("a notification has one to four buttons");
  return { id, title: text(spec.title, 120), body: text(spec.body, 500), actions };
}

/** Each notification's answer: the pressed button's text, "open" for a click
 *  on its body, or null once it is dismissed, replaced, taken down, or
 *  couldn't be shown (notifications off, or an unsigned app). */
export class Notices {
  private shown = new Map<string, { native: Native; answer: (pressed: string | null) => void }>();
  private make: (spec: NoticeSpec) => Native;
  private toFront: () => void;

  constructor(make: (spec: NoticeSpec) => Native, toFront: () => void) {
    this.make = make;
    this.toFront = toFront;
  }

  notify(spec: NoticeSpec): Promise<string | null> {
    this.close(spec.id);
    return new Promise((resolve) => {
      const native = this.make(spec);
      let answered = false;
      const answer = (pressed: string | null) => {
        if (answered) return;
        answered = true;
        if (this.shown.get(spec.id)?.native === native) this.shown.delete(spec.id);
        resolve(pressed);
      };
      // Electron passes { actionIndex }, and the index again after it.
      native.on("action", (details?: { actionIndex?: number }, index?: number) =>
        answer(spec.actions[details?.actionIndex ?? index ?? -1] ?? null),
      );
      native.on("click", () => {
        this.toFront();
        answer("open");
      });
      native.on("close", () => answer(null));
      native.on("failed", () => answer(null));
      this.shown.set(spec.id, { native, answer });
      native.show();
    });
  }

  /** Takes one down: its notify answers null. */
  close(id: string): void {
    const shown = this.shown.get(id);
    if (!shown) return;
    this.shown.delete(id);
    shown.native.close();
    shown.answer(null);
  }

  /** A new page asks again for what is still open. */
  closeAll(): void {
    for (const id of [...this.shown.keys()]) this.close(id);
  }
}

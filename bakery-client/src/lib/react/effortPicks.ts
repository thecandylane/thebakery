// An effort pick moves its control at once and reaches the runtime in the
// background. This is the part that sends: one write at a time, the latest
// pick last, and what the control shows when the runtime takes a pick,
// refuses it, or its answer is overtaken. No React in here, so
// `effortPicks.check.ts` runs the rules as the composer runs them.

/** What became of one write to the runtime. */
export type EffortWrite =
  /** The runtime took it; what it answered is already shown. */
  | "acknowledged"
  /** The runtime said no, or could not be asked (the reason is already shown). */
  | "refused"
  /** A newer read of the runtime came in meanwhile, or its session changed,
   *  so the answer was not used: whether the runtime took the pick is
   *  unknown until it is read again. */
  | "overtaken";

export type EffortHost = {
  /** Shows `value` as the effort; `null` is none chosen. */
  show(value: string | null): void;
  /** Writes `value` to the runtime. */
  write(value: string): Promise<EffortWrite>;
  /** Reads the runtime's own state and shows it. */
  read(): Promise<void>;
  /** The effort the runtime last answered with: `null` when it reports none,
   *  `undefined` before it has answered at all. */
  acknowledged(): string | null | undefined;
};

export class EffortPicks {
  private host: EffortHost;
  /** The latest pick the runtime has not taken yet. */
  private wanted: string | null = null;
  /** What the control showed before the first pick of a run. */
  private before: string | null = null;
  private running: Promise<void> | null = null;
  /** Changes with the conversation, so a run for the old one stops. */
  private epoch = 0;

  constructor(host: EffortHost) {
    this.host = host;
  }

  /** The sending that is running, for whatever must go after it. */
  get sending(): Promise<void> | null {
    return this.running;
  }

  /** `shown` is what the control shows now. */
  pick(value: string, shown: string | null): void {
    if (this.running === null) this.before = shown;
    this.wanted = value;
    this.host.show(value);
    this.running ??= this.run(this.epoch);
  }

  /** The conversation changed: a pick still waiting belonged to the old one,
   *  and the old one's answers count for nothing. */
  reset(): void {
    this.epoch++;
    this.wanted = null;
    this.running = null;
  }

  private async run(epoch: number): Promise<void> {
    try {
      while (this.wanted !== null && this.epoch === epoch) {
        const sent = this.wanted;
        let result: EffortWrite;
        try {
          result = await this.host.write(sent);
        } catch {
          result = "refused";
        }
        if (this.epoch !== epoch) return;
        // An overtaken answer rolls nothing back: the runtime is read, and
        // what it says is what the control shows.
        if (result === "overtaken") {
          await this.host.read().catch(() => {});
          if (this.epoch !== epoch) return;
        }
        // A pick made while this one was on its way is still to go; the
        // control keeps showing it, whatever became of this one.
        const newer = this.wanted !== sent ? this.wanted : null;
        if (newer !== null) {
          this.host.show(newer);
          continue;
        }
        this.wanted = null;
        if (result === "refused") {
          // Back to what the runtime holds. If it has not said, to what the
          // control showed before.
          const held = this.host.acknowledged();
          this.host.show(held === undefined ? this.before : held);
        }
      }
    } finally {
      if (this.epoch === epoch) this.running = null;
    }
  }
}

// A native session's model chooser (the operator's QA: the list Claude Code
// itself shows, in its own style): exactly what the live session advertises,
// never a remembered or hand-written list. Each model one row, its name, its
// id (a "[1m]" context variant reads as one) and its one-line description;
// the effort the session offers as a dot slider; then every other option it
// advertises, a context choice too, each with its own description. A pick
// goes to the session as it is (/agent/runtime/config); the session's answer
// is what shows. The effort is the one that moves at once: its dot is under
// the pointer the instant it is picked, and goes back if the session refuses.

import { CheckIcon } from "lucide-react";
import type { AcpConfigOption, AcpConfigValueOption, AcpRuntimeSnapshot } from "$lib/api.generated";
import { DialogContent, DialogTitle } from "@/components/ui/dialog";

const ROW = "flex w-full items-start gap-2.5 rounded-md px-2.5 py-2 text-left hover:bg-(--bg-chip) disabled:opacity-50 disabled:hover:bg-transparent";

/** The values in their groups, in the session's order. */
function grouped(values: AcpConfigValueOption[]): [string, AcpConfigValueOption[]][] {
  const out: [string, AcpConfigValueOption[]][] = [];
  for (const v of values) {
    const g = v.group_name ?? "";
    if (out.at(-1)?.[0] === g) out.at(-1)?.[1].push(v);
    else out.push([g, [v]]);
  }
  return out;
}

function Effort({ option, busy, onPick }: { option: AcpConfigOption; busy: boolean; onPick: (id: string) => void }) {
  const values = option.values;
  const at = values.findIndex((v) => v.id === option.current_value);
  const span = Math.max(1, values.length - 1);
  return (
    <div className="border-(--line-soft) border-t px-4 pt-3 pb-3.5">
      <div className="mb-2.5 flex items-center gap-2 text-[12.5px]">
        <span className="font-semibold text-(--fg)">{option.name || "Effort"}</span>
        <span className="flex-1" />
        <span className="text-(--fg-dim) text-[11.5px]">{values[at]?.name ?? "default"}</span>
      </div>
      <div aria-label={option.name || "Effort"} className="relative flex h-5 items-center justify-between" role="radiogroup">
        <span className="absolute inset-x-2 top-1/2 h-0.5 -translate-y-1/2 rounded-full bg-(--line)" />
        {at > 0 && <span className="absolute left-2 top-1/2 h-0.5 -translate-y-1/2 rounded-full bg-(--accent)" style={{ width: `calc((100% - 16px) * ${at / span})` }} />}
        {values.map((v, i) => (
          <button
            aria-checked={i === at}
            aria-label={v.name}
            className="relative grid size-4 place-items-center rounded-full disabled:opacity-50"
            disabled={busy}
            key={v.id}
            onClick={() => i !== at && onPick(v.id)}
            role="radio"
            title={v.description || v.name}
            type="button"
          >
            <span
              className={`block rounded-full transition-all ${i === at ? "size-3.5 bg-(--accent) shadow-[0_0_0_3px_color-mix(in_srgb,var(--accent)_25%,transparent)]" : i < at ? "size-2 bg-(--accent)" : "size-2 bg-(--fg-faint)"}`}
            />
          </button>
        ))}
      </div>
      <div className="mt-1.5 flex justify-between text-(--fg-faint) text-[10.5px]">
        <span>{values[0]?.name}</span>
        <span>{values.at(-1)?.name}</span>
      </div>
    </div>
  );
}

function Toggle({ option, busy, onPick }: { option: AcpConfigOption; busy: boolean; onPick: (on: boolean) => void }) {
  const on = option.current_value === true;
  return (
    <div className="flex items-center gap-3 border-(--line-soft) border-t px-4 py-2.5">
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="font-semibold text-(--fg) text-[12.5px]">{option.name}</span>
        {option.description && <span className="truncate text-(--fg-dim) text-[11.5px]">{option.description}</span>}
      </span>
      <button
        aria-checked={on}
        aria-label={option.name}
        className={`relative h-[18px] w-8 shrink-0 rounded-full disabled:opacity-50 ${on ? "bg-(--accent-solid)" : "bg-(--bg-chip)"}`}
        disabled={busy}
        onClick={() => onPick(!on)}
        role="switch"
        type="button"
      >
        <span className={`absolute top-0.5 size-3.5 rounded-full bg-(--bg-app) transition-all ${on ? "left-4" : "left-0.5"}`} />
      </button>
    </div>
  );
}

/** Any other choice the session offers: its values in a row. */
function Choice({ option, busy, onPick }: { option: AcpConfigOption; busy: boolean; onPick: (id: string) => void }) {
  return (
    <div className="flex flex-col gap-1.5 border-(--line-soft) border-t px-4 py-2.5">
      <span className="flex min-w-0 flex-col">
        <span className="font-semibold text-(--fg) text-[12.5px]">{option.name}</span>
        {option.description && <span className="truncate text-(--fg-dim) text-[11.5px]">{option.description}</span>}
      </span>
      <span className="flex flex-wrap gap-1">
        {option.values.map((v) => (
          <button
            aria-pressed={v.id === option.current_value}
            className={`h-6 rounded-[5px] border px-2 text-[11px] disabled:opacity-50 ${v.id === option.current_value ? "border-(--accent-border) bg-(--accent-sel) font-semibold text-(--fg)" : "border-(--line) text-(--fg-2) hover:bg-(--bg-chip)"}`}
            disabled={busy}
            key={v.id}
            onClick={() => v.id !== option.current_value && onPick(v.id)}
            title={v.description || undefined}
            type="button"
          >
            {v.name}
          </button>
        ))}
      </span>
    </div>
  );
}

export function NativeModelPicker({
  runtime,
  busy,
  loading,
  onApply,
  onClose,
  provider,
  providerWaiting,
  providers,
  onProvider,
  turnRunning,
  onEffort,
}: {
  /** A Claude seat's provider now, and the ones to offer (keys saved). */
  provider?: string | null;
  /** The provider a switch asked for mid-turn moves it to when the turn
   *  ends; the turn runs on its provider meanwhile. */
  providerWaiting?: string | null;
  providers?: readonly { id: string; label: string }[];
  /** Moves the seat to another provider: mid-turn, when the turn ends;
   *  back to its own, the waiting switch is cancelled. */
  onProvider?: (id: string) => void;
  /** The seat's turn is running: a provider picked now waits for its end. */
  turnRunning?: boolean;
  /** The live session, or null while it starts. */
  runtime: AcpRuntimeSnapshot | null;
  busy: boolean;
  loading: boolean;
  /** The session's answer (falsy when it refused or went away). */
  onApply: (optionId: string, value: string | boolean) => Promise<unknown>;
  onClose: () => void;
  /** An effort pick: the control has moved by the time this returns, and the
   *  session is told in the background. */
  onEffort: (value: string) => void;
}) {
  const options = runtime?.config_options ?? [];
  const model = options.find((o) => o.category === "model" && o.kind === "select");
  const effort = options.find((o) => o.category === "thought_level" && o.kind === "select");
  const rest = options.filter((o) => o !== model && o !== effort && (o.kind === "boolean" || o.kind === "select"));
  const who = runtime?.runtime ? `${runtime.runtime.title || runtime.runtime.name} ${runtime.runtime.version}` : "";
  // When a waiting switch lands: idle, the node restarted under the turn.
  const when = turnRunning ? "when this turn ends" : "as its next turn starts";
  return (
    <DialogContent className="max-h-[90dvh] max-w-sm gap-0 overflow-y-auto border-(--line) bg-(--bg-raised) p-0 text-(--fg-body)">
      <DialogTitle className="flex items-baseline gap-2 px-4 pt-3.5 pb-2 font-semibold text-(--fg) text-sm">
        Model
        {who && <span className="truncate font-normal text-(--fg-faint) text-[10.5px]">{who}</span>}
      </DialogTitle>
      {!model ? (
        <p className="px-4 pb-4 text-(--fg-dim) text-xs">{loading ? "Starting the session…" : "This session offers no model choice."}</p>
      ) : (
        <div className="max-h-[52vh] overflow-auto px-1.5 pb-1.5" role="radiogroup">
          {grouped(model.values).map(([group, values]) => (
            <div key={group || "models"}>
              {group && <div className="px-2.5 pt-2 pb-1 font-bold text-(--fg-dim) text-[10px] tracking-[.08em]">{group.toUpperCase()}</div>}
              {values.map((v) => (
                <button
                  aria-checked={v.id === model.current_value}
                  className={`${ROW} ${v.id === model.current_value ? "bg-(--accent-sel)" : ""}`}
                  disabled={busy}
                  key={v.id}
                  onClick={() => {
                    if (v.id === model.current_value) return onClose();
                    void onApply(model.id, v.id).then((ok) => ok && !effort && rest.length === 0 && onClose());
                  }}
                  role="radio"
                  type="button"
                >
                  <CheckIcon className={`mt-0.5 size-3.5 shrink-0 text-(--accent) ${v.id === model.current_value ? "opacity-100" : "opacity-0"}`} />
                  <span className="flex min-w-0 flex-col">
                    <span className="flex min-w-0 items-baseline gap-1.5">
                      <span className="truncate font-medium text-(--fg) text-[13px]">{v.name}</span>
                      {v.id !== v.name && <span className="min-w-0 truncate font-mono text-(--fg-faint) text-[10.5px]">{v.id}</span>}
                    </span>
                    {v.description && <span className="truncate text-(--fg-dim) text-[11.5px]">{v.description}</span>}
                  </span>
                </button>
              ))}
            </div>
          ))}
        </div>
      )}
      {provider && providers && onProvider && (
        <div className="border-(--line-soft) border-t px-4 py-2.5" data-testid="seat-provider">
          <div className="mb-1.5 font-semibold text-(--fg) text-[12.5px]">Provider</div>
          <span className="flex flex-wrap gap-1.5" role="group">
            {providers.map((p) => (
              <button
                aria-pressed={p.id === provider}
                className={`h-6 rounded-[5px] border px-2 text-[11px] disabled:opacity-50 ${p.id === provider ? "border-(--accent-border) bg-(--accent-sel) font-semibold text-(--fg)" : p.id === providerWaiting ? "border-(--accent-border) border-dashed text-(--fg)" : "border-(--line) text-(--fg-2) hover:bg-(--bg-chip)"}`}
                disabled={busy}
                key={p.id}
                onClick={() => p.id !== (providerWaiting ?? provider) && onProvider(p.id)}
                title={
                  p.id === providerWaiting
                    ? `Switches to it ${when}`
                    : p.id === provider && providerWaiting
                      ? "Stay on it: cancels the switch"
                      : turnRunning && p.id !== provider
                        ? "Switches when this turn ends; the turn runs on"
                        : undefined
                }
                type="button"
              >
                {p.label}
              </button>
            ))}
          </span>
          {providerWaiting && (
            <p className="mt-1.5 text-(--fg-dim) text-[11px]" data-testid="seat-provider-waiting">
              Switches to {providers.find((p) => p.id === providerWaiting)?.label ?? providerWaiting} {when}.
            </p>
          )}
        </div>
      )}
      {effort && <Effort busy={busy} onPick={onEffort} option={effort} />}
      {rest.map((o) =>
        o.kind === "boolean" ? (
          <Toggle busy={busy} key={o.id} onPick={(on) => void onApply(o.id, on)} option={o} />
        ) : (
          <Choice busy={busy} key={o.id} onPick={(id) => void onApply(o.id, id)} option={o} />
        ),
      )}
    </DialogContent>
  );
}

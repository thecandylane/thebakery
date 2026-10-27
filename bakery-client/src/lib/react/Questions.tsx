// Questions an agent asked the human. They are answered in one card above
// the composer that steps through them; once submitted, the thread keeps a
// record of every question, what it offered, and what was chosen. One shape
// for every runtime: an HTTP seat's `ask_user` and a native runtime's form
// (Claude's AskUserQuestion) both arrive as `agent.ask.pending`.

import { type KeyboardEvent, type ReactNode, useCallback, useRef, useState } from "react";
import { CheckIcon, ChevronRightIcon, MessageCircleQuestionIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Input } from "@/components/ui/input";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { cn } from "@/lib/utils";
import type { AskAnswer, AskQuestion, TranscriptItem } from "$lib/transcript";
import type { AgentPanelActions } from "./contract";

/** The value "Other" holds in a single-select group — never an option label. */
const OTHER = "\u0000other";

type Draft = { picks: string[]; other: string; otherOn: boolean };

const emptyDraft = (): Draft => ({ picks: [], other: "", otherOn: false });
const isAnswered = (d: Draft) => d.picks.length > 0 || (d.otherOn && d.other.trim() !== "");

function toAnswer(d: Draft): AskAnswer {
  const other = d.otherOn ? d.other.trim() : "";
  return other ? { picks: d.picks, other } : { picks: d.picks };
}

/** One option: its mark, its label and description, and — in the card — the
 *  number key that picks it. In the thread's record it is read-only. */
function OptionRow({ label, description, number, children }: {
  label: string;
  description?: string;
  number?: number;
  children: ReactNode;
}) {
  return (
    <label
      className={cn(
        "flex items-start gap-2.5 rounded-md px-2 py-1.5 has-[[data-state=checked]]:bg-muted",
        number !== undefined && "cursor-pointer hover:bg-muted/60",
      )}
    >
      <span className="mt-0.5 flex">{children}</span>
      <span className="min-w-0 flex-1">
        <span className="block text-foreground">{label}</span>
        {description && <span className="block text-muted-foreground text-xs">{description}</span>}
      </span>
      {number !== undefined && number <= 9 && <kbd className="font-mono text-[10px] text-muted-foreground">{number}</kbd>}
    </label>
  );
}

/** The questions a parked turn is waiting on, one at a time: pick an option
 *  or type your own answer, then submit them together. Skip answers none. */
export function QuestionCard({ hash, questions, actions }: {
  hash: string;
  questions: readonly AskQuestion[];
  actions: AgentPanelActions;
}) {
  const [index, setIndex] = useState(0);
  const [drafts, setDrafts] = useState<Draft[]>(() => questions.map(emptyDraft));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  // A ref, not `busy`: two clicks in the same tick both see the old state.
  const inFlight = useRef(false);

  const question = questions[index];
  const draft = drafts[index];
  const last = index === questions.length - 1;
  const edit = (change: (d: Draft) => Draft) => setDrafts((all) => all.map((d, i) => (i === index ? change(d) : d)));

  const submit = useCallback(
    async (answers: AskAnswer[]) => {
      if (inFlight.current) return;
      inFlight.current = true;
      setBusy(true);
      setError("");
      const out = await actions.answer(hash, answers);
      if (out.status === "error") setError(out.error);
      setBusy(false);
      inFlight.current = false;
    },
    [actions, hash],
  );
  const next = () => (last ? void submit(drafts.map(toAnswer)) : setIndex(index + 1));

  const pick = (label: string) =>
    edit((d) =>
      question.multiSelect
        ? { ...d, picks: question.options.map((o) => o.label).filter((l) => (l === label) !== d.picks.includes(l)) }
        : { ...d, picks: [label], otherOn: false },
    );
  const typeOther = (text: string) =>
    edit((d) => ({
      ...d,
      other: text,
      otherOn: text.trim() !== "" || (question.multiSelect && d.otherOn),
      picks: !question.multiSelect && text.trim() !== "" ? [] : d.picks,
    }));

  // Enter moves on (submits on the last question); 1–9 picks an option.
  // Buttons keep their own Enter, and the text box keeps its digits.
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.nativeEvent.isComposing || e.metaKey || e.ctrlKey || e.altKey) return;
    const target = e.target as HTMLElement;
    const role = target.getAttribute("role");
    if (target instanceof HTMLButtonElement && role !== "radio" && role !== "checkbox") return;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (!busy) next();
      return;
    }
    const n = Number(e.key);
    if (!(target instanceof HTMLInputElement) && Number.isInteger(n) && n >= 1 && n <= question.options.length) {
      e.preventDefault();
      pick(question.options[n - 1].label);
    }
  };

  if (!question) return null;
  const other = (
    <div className="flex items-center gap-2.5 rounded-md px-2 py-1 has-[[data-state=checked]]:bg-muted">
      <span className="flex">
        {question.multiSelect ? (
          <Checkbox aria-label="Other" checked={draft.otherOn} onCheckedChange={(c) => edit((d) => ({ ...d, otherOn: c === true }))} />
        ) : (
          <RadioGroupItem aria-label="Other" value={OTHER} />
        )}
      </span>
      <Input
        className="h-7 text-xs md:text-xs"
        onChange={(e) => typeOther(e.currentTarget.value)}
        placeholder="Other — type your own answer"
        value={draft.other}
      />
    </div>
  );

  return (
    <div className="bakery-ai-control-lane" data-testid="questions">
      <div
        aria-label="The agent's questions"
        className="bakery-ai-surface flex flex-col gap-2.5 rounded-xl border border-border bg-card p-3 text-sm"
        onKeyDown={onKeyDown}
        role="group"
      >
        <div className="flex flex-wrap items-center gap-1">
          <MessageCircleQuestionIcon className="mr-0.5 size-3.5 shrink-0 text-muted-foreground" />
          {questions.length === 1 ? (
            <span className="text-muted-foreground text-xs">{question.header ?? "Question"}</span>
          ) : (
            <>
              {questions.map((q, i) => (
                <button
                  aria-current={i === index ? "step" : undefined}
                  className={cn(
                    "inline-flex h-6 items-center gap-1 rounded-md px-2 text-xs transition-colors",
                    i === index ? "bg-accent text-accent-foreground" : "text-muted-foreground hover:bg-muted",
                  )}
                  key={i}
                  onClick={() => setIndex(i)}
                  type="button"
                >
                  {isAnswered(drafts[i]) && <CheckIcon className="size-3" />}
                  {q.header ?? `Question ${i + 1}`}
                </button>
              ))}
              <span className="ml-auto text-muted-foreground text-xs tabular-nums">
                {index + 1} of {questions.length}
              </span>
            </>
          )}
        </div>

        <div className="font-medium text-foreground" data-testid="question-text">
          {question.question}
        </div>

        {question.options.length === 0 ? (
          <Input
            aria-label="Your answer"
            onChange={(e) => edit((d) => ({ ...d, other: e.currentTarget.value, otherOn: true }))}
            placeholder="Type your answer"
            value={draft.other}
          />
        ) : question.multiSelect ? (
          <div className="flex flex-col gap-0.5">
            {question.options.map((o, i) => (
              <OptionRow description={o.description} key={o.label} label={o.label} number={i + 1}>
                <Checkbox checked={draft.picks.includes(o.label)} onCheckedChange={() => pick(o.label)} />
              </OptionRow>
            ))}
            {other}
          </div>
        ) : (
          <RadioGroup
            className="gap-0.5"
            onValueChange={(v) => (v === OTHER ? edit((d) => ({ ...d, picks: [], otherOn: true })) : pick(v))}
            value={draft.otherOn ? OTHER : (draft.picks[0] ?? "")}
          >
            {question.options.map((o, i) => (
              <OptionRow description={o.description} key={o.label} label={o.label} number={i + 1}>
                <RadioGroupItem value={o.label} />
              </OptionRow>
            ))}
            {other}
          </RadioGroup>
        )}

        <div className="flex items-center gap-1.5">
          <Button
            className="text-muted-foreground"
            disabled={busy}
            onClick={() => void submit(questions.map(() => ({ picks: [] })))}
            size="sm"
            title="Answer none of them; the agent carries on without"
            variant="ghost"
          >
            Skip
          </Button>
          {error && <span className="min-w-0 truncate text-destructive text-xs">{error}</span>}
          <div className="ml-auto flex gap-1.5">
            {index > 0 && (
              <Button onClick={() => setIndex(index - 1)} size="sm" variant="outline">
                Back
              </Button>
            )}
            <Button data-testid={last ? "questions-submit" : "questions-next"} disabled={busy} onClick={next} size="sm">
              {last ? (busy ? "Sending…" : "Submit") : "Next"}
              {!last && <ChevronRightIcon />}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}

/** An answer recorded as text alone (an older client): an exact option label
 *  of a lone question is that pick; anything else is the human's own words. */
function answersFromText(questions: readonly AskQuestion[], text: string): AskAnswer[] {
  return questions.map((q, i) =>
    i > 0
      ? { picks: [] }
      : questions.length === 1 && q.options.some((o) => o.label === text)
        ? { picks: [text] }
        : { picks: [], other: text },
  );
}

function AnsweredQuestion({ question, answer }: { question: AskQuestion; answer: AskAnswer }) {
  const other = answer.other?.trim() ?? "";
  const skipped = answer.picks.length === 0 && other === "";
  // A single-select pick with words of its own: the words are its note.
  const note = !question.multiSelect && answer.picks.length > 0 ? other : "";
  const rows = question.options.map((o) => (
    <OptionRow
      description={answer.picks.includes(o.label) && note ? `${o.description ? `${o.description} — ` : ""}note: ${note}` : o.description}
      key={o.label}
      label={o.label}
    >
      {question.multiSelect ? (
        <Checkbox checked={answer.picks.includes(o.label)} disabled />
      ) : (
        <RadioGroupItem disabled value={o.label} />
      )}
    </OptionRow>
  ));
  const otherRow = other && !note && (
    <OptionRow description={other} label="Other">
      {question.multiSelect ? <Checkbox checked disabled /> : <RadioGroupItem disabled value={OTHER} />}
    </OptionRow>
  );
  return (
    <div className="flex flex-col gap-0.5">
      <div className="font-medium text-foreground">{question.question}</div>
      {question.options.length === 0 ? (
        other && <div className="px-2 text-foreground">{other}</div>
      ) : question.multiSelect ? (
        <div className="flex flex-col gap-0.5">
          {rows}
          {otherRow}
        </div>
      ) : (
        <RadioGroup className="gap-0.5" disabled value={answer.picks[0] ?? (other ? OTHER : "")}>
          {rows}
          {otherRow}
        </RadioGroup>
      )}
      {skipped && <div className="px-2 text-muted-foreground text-xs">skipped</div>}
    </div>
  );
}

/** The thread's record of an ask: waiting on the card above the composer,
 *  stopped, or answered — every question with what it offered and what was
 *  chosen, like the card it was answered in. */
export function AskRecord({ item }: { item: Extract<TranscriptItem, { kind: "ask" }> }) {
  const count = item.questions.length;
  const noun = `${count} question${count === 1 ? "" : "s"}`;
  if (item.answer === undefined) {
    return (
      <div className="bakery-callout" data-callout={item.cancelled ? "quiet" : "ask"} data-testid="ask">
        <div className="bakery-callout-head">
          <MessageCircleQuestionIcon className="bakery-callout-icon size-3.5" />
          <span className="bakery-callout-title">Questions</span>
          <span>{item.cancelled ? "stopped before an answer" : `waiting on your answer · ${noun}`}</span>
        </div>
      </div>
    );
  }
  const answers = item.answers?.length === count ? item.answers : answersFromText(item.questions, item.answer);
  return (
    <Collapsible className="bakery-callout" data-callout="quiet" data-testid="ask" defaultOpen>
      <CollapsibleTrigger className="bakery-callout-head group w-full text-left">
        <MessageCircleQuestionIcon className="bakery-callout-icon size-3.5" />
        <span className="bakery-callout-title">Questions</span>
        <span>Answered · {noun}</span>
        <ChevronRightIcon className="size-3.5 transition-transform group-data-[state=open]:rotate-90" />
      </CollapsibleTrigger>
      <CollapsibleContent className="mt-1 flex flex-col gap-3">
        {item.questions.map((q, i) => (
          <AnsweredQuestion answer={answers[i]} key={i} question={q} />
        ))}
      </CollapsibleContent>
    </Collapsible>
  );
}

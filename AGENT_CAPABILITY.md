# The Bakery — Agent Capability & Execution Layer

**Locked Specification v0.3**
thecandylane | July 2026

> Companion to [KNOWLEDGE_HEART.md](KNOWLEDGE_HEART.md) (locked v0.3.3) and [CONTROL_PLANE.md](CONTROL_PLANE.md) (locked v0.8). Where they touch (streams, L3, ingestion gate, events), those documents are authoritative.

---

## 1. Purpose & Governing Philosophy

This layer gives agents the power to inspect, edit, run, test, and debug code.

**Governing rule:**

- The Bakery owns the engineering process, structure, context assembly, memory, tooling, safety, evaluation, and observability.
- Models supply actionable intelligence inside that structure.
- The harness normalizes a **model-agnostic interface** so models are **swappable without re-architecting** the system.
- Context assembly is **model-aware** (it adapts packing and feature use to the target model's context window and capabilities).
- Capability, reliability, and output quality still track the underlying model. **We do not claim outcome equivalence across models.**

High-level thinking and steering happen between you and The Baker / the project agent. The harness does the rest of the software engineering.

---

## 2. Actors

- **The Baker** — Factory-level agent. Always knows it is The Baker, regardless of model. Can orchestrate across projects (subject to the elevated UI gate and the ingestion gate).
- **Project / Worktree Agent** — One primary agent per active worktree. Strictly scoped to that worktree's code, tools, skills, connectors, and context. Does not know factory topology, other projects, or Canon internals (only generic expert-engineer priming + its own granted capability surface).
- **Sub-agents** — Disposable helpers spawned by a project agent or The Baker.
  - Aware of what their parent is doing and what they themselves are doing.
  - Receive only the context explicitly given to them.
  - Heart access is **at most the parent's access** and may be refined downward.
  - Can use tools under the same permission modes as their parent scope.

---

## 3. Context Isolation & Assembly

- Project/worktree agents are strictly isolated to their own project.
- Cross-project knowledge is injected only when the Bakery's context assembler decides it is relevant (or when explicitly requested).
- There is an in-app surface for managing system prompts / priming.
- On each turn the Bakery builds a context package from:
  - Current worktree code
  - Relevant Heart slices (including L3)
  - Current plan and recent events
  - Allowed capabilities
  - Selected cross-project patterns when useful
- Context assembly is **model-aware**: packing density, summarization, and feature use adapt to the target model's context window and tool-calling characteristics.
- Model switching is supported without system changes. Behavior and quality will still vary with the model; the interface and process stay stable.

---

## 4. Permission & Auto-Run Modes

Four explicit modes (Bakery policy — identical regardless of model):

| Mode | Behavior |
|------|----------|
| **Manual** | Every action requires human approval |
| **Plan** | Auto-read only. No writes or edits |
| **Auto Edit** | Can edit files automatically. Must ask before running agent-invoked commands |
| **Auto Go** | Full autonomy within granted capabilities (still subject to irreversible-action gates) |

Agents may request actions that are currently denied. You answer yes / no / sometimes (+ reasons). Those decisions become part of the learned policy. Finer-grained rules can be refined during implementation.

**Important distinction:** Harness-invoked evaluation (tests, linters, type-checkers run by the Bakery after a unit of work) is **not** treated as an agent-invoked command. It is not blocked by Auto Edit's "ask before commands" rule.

---

## 5. Capability Families

Capabilities the Bakery can grant (new ones added only through the ingestion gate):

- Code Intelligence (LSP-powered; feeds and reads L3)
- Editing & Transformation (always produces diffs → events)
- Execution & Observation (sandboxed shell, builds, processes)
- Debugging (DAP, agent-driven)
- Testing
- Knowledge & Documentation (Heart queries + live doc fetch)
- Version Control (policy-controlled git operations)
- Browser / UI Observation (future)
- Meta / Factory (primarily Baker)

---

## 6. Observation & Tool Result Handling

- Raw tool output does not automatically flood model context.
- The Bakery maintains a structured observation / tool-result store that agents can query.
- Models receive what happened, why it matters, and where to look — not every raw byte by default.
- Exact design is left open for research against existing strong harnesses.

---

## 7. Evaluation Feedback

- Configurable per project/worktree.
- Default preference: run relevant tests / linters / checks after a coherent unit of work (change set, plan step, or short sprint).
- These runs are harness-invoked, not agent-invoked commands.
- Results are written into the observation store and emitted as events.

---

## 8. Safety & Reversibility

Safety is structural, not prompt-based.

**Two different kinds of reversibility:**

- **Knowledge-plane reversibility (high)** — Event-sourcing lets us reconstruct memory, reject diffs, and roll back Heart state.
- **World side-effects (not automatically reversible)** — Shell commands, git push, database changes, paid/external API calls, etc. cannot be undone by the event log.

**Rule:** Actions are classified as reversible or irreversible. Irreversible actions are hard-gated regardless of auto-run mode. Auto Go does not mean automatic git push or unconstrained destructive operations. ("Hard-gated" means *requires explicit authorization* — the finer permission language may later add tightly-scoped pre-authorizations; it never means model self-attestation.)

**Additional protections:**

- Agents only receive permitted data and capabilities.
- Schema validation / allowlist gating sits in front of dangerous operations.
- Everything is monitored and logged.

---

## 9. Relationship to Other Layers

- All meaningful actions produce events that flow into the Heart streams and the Control Plane activity queue.
- LSP / structural results feed L3.
- Edits always appear as diffs that can be accepted/rejected (producing events).
- Capabilities are subject to the Heart's ingestion gate before they become available.

---

## 10. What This Specification Freezes

**Frozen**

- Harness owns process and normalizes a model-agnostic interface; models are swappable without system re-architecture; context assembly is model-aware; quality still tracks the model
- Strict project/worktree isolation for project agents
- The Baker always knows it is The Baker
- Four auto-run modes (Manual / Plan / Auto Edit / Auto Go)
- Harness-invoked evaluation is distinct from agent-invoked commands
- Sub-agent model (disposable, parent-aware, limited context, Heart access ≤ parent and refinable down)
- Capability families listed above
- Observation store instead of raw tool-output spam
- Post-unit-of-work evaluation preference
- Knowledge-plane reversibility via events; irreversible world actions are hard-gated independent of mode
- All meaningful actions produce events consumable by Heart and Control Plane

**Open / to be refined during implementation or next slices**

- Exact observation / tool-result cache design
- Concrete context-assembly pipeline details
- Capability registration & runtime lifecycle
- Sandbox implementation
- Model adapter interface details
- Ungraceful node handoff / lease + fence mechanism
- Finer permission language beyond the four modes (including scoped pre-authorization of irreversible actions)

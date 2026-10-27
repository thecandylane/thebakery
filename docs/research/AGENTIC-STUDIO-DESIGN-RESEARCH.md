# Bakery Agentic Studio Design

## Recommendation

Make Bakery a **compact work console with a chat-first studio**, not a dashboard with an expanding collection of cards. Factory Floor should answer what exists and what needs attention. Baker should turn conversation into inspectable work. A project studio should let the human talk, edit, run, steer, and review without losing context.

The strongest combination is Zed's project-grouped agent navigation, Cursor's agent-first workspace, oh-my-pi's agent roster and steering interactions, Linear's component consistency, and Primer's semantic design foundations. Use Carbon selectively for resource tables. Keep Bakery's existing Svelte, Monaco, Dockview, xterm, and node-owned runtime. These are recommendations to adapt interaction patterns, not instructions to merge those products or adopt their data models.[^zed][^cursor][^omp][^linear][^primer-tokens][^carbon]

The immediate design goal is not more visible capabilities. It is making the capabilities already built understandable, reachable, and dependable. A professional interface should consistently answer:

1. Which project, socket, and agent am I operating?
2. What is actually happening, and what needs me?
3. What can I do next without losing my work?
4. What changed, what was checked, and what am I approving?

This is a research brief and proposed design direction, not a new signed implementation slice. It does not change the existing product contract or authorize code changes.

## Evidence and the current checkpoint

Research was conducted on 9 September 2026 using official product documentation, first-party design systems, public repository documentation, the supplied UI screenshots, and selected local Bakery/reference sources. Documentation describes intended behavior; it is not a comparative usability benchmark. VS Code's Agents window is explicitly a preview. Refero was requested as an inspiration source, but its individual screens were not inspectable through the available browser connection. No recommendations below claim pixel measurements from Refero.[^vscode][^refero]

The Baker implementation represents substantial progress toward the intended operating loop. Its slice header correctly calls it **Checkpoint 1 built, awaiting acceptance**, not the entire multi-agent factory completed. Permanent identity, task lifecycle, operator dispatch, transport delivery, background execution, and review are the right connected pieces. The reported passing suites are valuable evidence from the implementation session; this research pass did not rerun them or independently audit all reported fixes.

Two qualifications matter before unattended operation. The Baker Control slice of the time and the [MCP module](../../crates/bakery-node/src/mcp.rs) explicitly document that ordinary loopback HTTP routes remain unauthenticated. MCP scoping therefore does not prevent an ACP child from reaching privileged operations through another local channel. Calling this pre-existing does not make it irrelevant to the new Baker/worker distinction. Close that bypass before treating scoped agent authority as enforced. A shared privileged credential handed to every child would not solve the distinction.

The operator's real-account smoke and browser coverage of the new task/review journey are also outstanding. Dependent-task ancestry, shipping/integration, and plain-directory task materialization are explicitly deferred. Consequently, a successful single-task demonstration is not yet proof that five interdependent tasks can be integrated correctly, or that non-Git tasks have equivalent execution support. These are concrete acceptance boundaries, not reasons to reopen accepted editor or docking work.

## What to borrow, and what not to borrow

| Reference | Useful evidence or pattern | Bakery application | Boundary |
|---|---|---|---|
| Zed Parallel Agents | Project-grouped threads, visible activity, agentic/classic layouts | Project → sockets; activity without opening every conversation | Do not adopt Git-dependent project identity |
| Cursor Agents Window | Agent-first workspace with files and review accessible in context | Chat-first studio with tools opened alongside | Do not treat its marketing website as the IDE specification |
| VS Code Agents window, preview | Sessions shared with the regular editor; active session determines relevant tools | Different views of the same durable work, not separate session systems | Preview behavior is inspiration, not a stability guarantee |
| oh-my-pi Agent Hub | Roster, inspector, transcript switching, steering, return to parent | Baker tracks workers; human enters one, steers, then returns | Do not copy destructive shortcuts or lifecycle semantics blindly |
| Linear | Consistent navigation, alignment, density, component behavior | Shared rows, headers, menus, and selected states | Avoid turning every socket into an issue-tracker card |
| Primer | Semantic tokens, progressive disclosure, actionable empty states | One theme vocabulary and predictable controls | Product patterns, not GitHub branding or Git-only workflows |
| Carbon | Structured tables with filtering, density, and expandable detail | Floor task/resource lists when comparison matters | Do not import a heavyweight dashboard shell |

Zed's documentation is particularly close to Bakery's need to work across projects: threads are organized by project and expose status and agent information. Its layouts make agent-first and editor-first use compatible rather than forcing separate applications.[^zed] Cursor similarly offers an agent-first workspace with review and file access, while VS Code's preview explicitly shares sessions with the editor's Chat view.[^cursor][^vscode]

Oh-my-pi supplies a more specific interaction to reuse than a screenshot: inspect a worker, switch the transcript and composer to that worker, send steering, and return without stopping its work. Its roster separates activity from usage and leaves unavailable usage unknown. Bakery can implement this through its own durable agent and task identities.[^omp]

Hermes and DeepSeek are stronger architectural references than visual specifications. Hermes documents persistent memory, skills, and agent delegation. DeepSeek's harness documents a plugin-oriented design and labels itself developer preview. Study the relevant seams for future knowledge and extensibility work; neither establishes that a new dashboard produces reliable learning or orchestration.[^hermes][^deepseek]

DockPanel is a useful future operations reference because it organizes application, service, and infrastructure management around concrete jobs. Moon is useful for project configuration and task relationships. Neither should become a mandatory first-run stack questionnaire for someone creating a chat-focused project.[^dockpanel][^moon]

## Interpret the saved style references correctly

The saved Cursor design reference (since removed from the repo) describes a marketing website: editorial typography, hero sections, pricing layouts, and large section spacing. Its restraint, neutral surfaces, and limited accent use are useful. Its marketing proportions are not appropriate defaults for file trees, terminals, task rosters, or a daily IDE. Do not copy proprietary font assets merely because the analysis names them.

The saved taste reference (also removed) contains useful guidance about hierarchy, complete states, semantic tokens, and coherent components. Its high-variance, spacious, motion-oriented defaults need a deliberate utility-workspace override. Dense information and quiet motion fit this product better than expressive layouts. Tables and compact lists are appropriate when users compare work across agents.

Bakery already has a centralized [theme.css](../../bakery-client/src/lib/styles/theme.css), with bridges into Monaco and xterm and direct variables for Dockview. Do not create another theme origin. Audit component use of that source, improve the semantic vocabulary where necessary, and make shell, editor, terminal, popovers, and diffs respond together. A separate user-selected syntax theme can be an explicit override later, not an accidental second palette.

Linear's redesign is useful because it worked through navigation, panel structure, density, and component consistency together. Primer's token guidance distinguishes primitive values from semantic and component roles. Those patterns support refining Bakery's existing system, not replacing it with unrelated color literals.[^linear][^primer-tokens]

## One product, three working contexts

Keep the approved sidebar structure: Factory Floor, Baker, Projects → sockets, Ungrouped, and the settings dial. Retired is a separate view. Do not introduce an additional branch hierarchy or a global approval queue disguised as a dashboard.

The three principal contexts should share selection, terminology, components, and live state, while answering different questions:

| Context | Main question | Primary surface | Secondary surface |
|---|---|---|---|
| Factory Floor | What exists, and what needs my attention? | Projects and live work | Selected project/task details |
| Baker | What should the factory do next? | Permanent Baker conversation | Work-order/task roster |
| Project studio | How do I carry out and assess this work? | Agent conversation | Code, Review, terminals, context |

A pane is a view, not an owner of work. Closing one must not silently cancel an agent, retire a socket, or destroy a terminal. Switching the model must not create a different Baker identity. These existing product distinctions should become visible interaction rules throughout the application.

Canonical topology does not mean every detail must live in one enormous client object. Task details and review artifacts can have their own projections. They must join through durable IDs, use the same lifecycle meanings, and avoid independently guessing project membership or active counts.

### Factory Floor: an operational list, not a wall of cards

Use a compact page header with clear creation actions: create a project, open an existing folder, or start an ungrouped socket. The primary list should show projects and relevant active work. Offer explicit views or filters for active work, work needing input, review-ready work, recent items, and retired items. Counts must describe the current category; a retired count must not appear to be the number of active sidebar sessions.

Task rows should prioritize title, project, actual phase, latest meaningful activity, and the next available action. Model, branch, root, and counters belong in secondary detail where useful. Do not show four equally prominent chips merely because four fields exist. Keep row positions stable during token streaming; activity should update without continuously re-sorting the page.

An attention indicator may deep-link to a task's conversation or Review pane. It should not enable cross-session sign-off from a detached global queue. The review remains attached to the task and revision the user is approving.

Show nodes and services as a compact, expandable resource section rather than making every resource a permanent navigation destination. Carbon's table guidance supports searchable, filterable, expandable resource lists; it does not imply every Floor section needs a table.[^carbon]

Empty states are part of this work. A new project should offer “New file,” “New folder,” and “Open folder” where supported, with a visible root. A rootless chat project should explain that files require adding a root, while allowing conversation immediately. “No files,” “no matching files,” “root unavailable,” and “still loading” are different states, not one blank rectangle. Primer explicitly treats these empty-state purposes differently.[^primer-empty]

### Baker: conversation with an inspectable work order

Use the same complete composer as project agents, not a branch-name form or a reduced chat widget. Baker's permanent identity and factory scope belong in its header. The model is an implementation choice shown in the composer, not the name of the conversation.

When Baker decomposes a request, show a concise work order linked to durable tasks: scope, dependencies, worker, phase, latest activity, and next human action. Detailed transcripts belong in the task's studio. This keeps five concurrent tasks inspectable without rendering five full conversations in the Baker page.

Selecting a task should preserve a clear way back to Baker and the work order. Steering must have an observable receipt: queued, delivered, rejected, or requiring a restart, according to the actual backend. A message sitting in an inbox must not be presented as already consumed by the worker. This follows the useful roster-to-worker interaction in oh-my-pi while preserving Bakery's lifecycle.[^omp]

Completion is not synonymous with approval, integration, or shipping. Baker should explain which stage is complete and which remains. If integration is not implemented, “Awaiting review” and “Signed off” are honest; “Shipped” is not.

### Studio: chat first, working tools beside it

A fresh socket opens its configured conversation first. A returning socket restores the user's layout. Code opens to the right, Review opens in an additional right-hand panel, and terminals open along the bottom. None should replace the active pane's content unexpectedly. Existing user layouts should not be reset simply to show off new defaults.

The Code surface combines file tree, tabs, editor, and explicit file-creation actions. Project, root, and branch information should appear once at the appropriate shell level, with additional detail available on demand. A selected file's full path can be inspectable without putting a long path into every header.

Keep Dockview. Its documentation explains that visibility-related DOM detachment can reset DOM-managed state, whereas persistent rendering has a cost because hidden content remains mounted. Use the existing version's lifecycle hooks deliberately and suspend expensive hidden rendering. Do not assume “always mounted” makes every state or performance issue disappear.[^dockview]

Close, hide, stop, cancel, and retire need different commands. Preserve drafts and editor models when switching views. Restore focus after closing an active panel without stealing focus after a background close. Test keyboard focus and native Tauri drag-and-drop as well as browser mouse actions. The earlier WKWebView-specific failure makes desktop verification a requirement, not an optional aesthetic check.

## The composer is the control surface

The screenshots supplied by the operator describe a compact, capable composer. Implement that as a coherent shared component, including Baker, rather than adding isolated dropdowns to each chat implementation.

The message field is primary. Its adjacent controls should expose attachments, supported commands, write mode, actual model, supported effort, and context/usage. Put advanced options in anchored popovers without hiding common actions behind a general settings page. The role belongs to agent identity; account/provider/model selection belongs to execution configuration.

Slash commands should be discoverable by typing `/` and available through a command menu. Advertise only implemented commands. Their effects need precise language:

- `/compact` must explain what context is summarized and what history remains stored.
- `/clear` must distinguish starting a fresh working context from deleting durable history.
- Plan mode must change actual execution behavior, not just put a label on unrestricted work.
- Model and effort choices must reflect the selected account and adapter's supported capabilities.

Do not invent a uniform effort slider for providers that expose different controls. Do not substitute token counts for subscription quota. Context occupancy, turn token usage, and account rate limits are separate measurements; show source, freshness, and unknown values appropriately.

While work runs, show factual progress: current reported activity, tool name, elapsed time, latest output, and cancel/steer affordances. Silence should be represented honestly as time since the last reported event. Do not manufacture percentages or imply access to hidden reasoning. Human-AI interaction guidance emphasizes understandable capability and correction; transparent operational state is more useful here than theatrical “thinking” text.[^hai]

Queueing a message and interrupting a turn are different operations. Cursor's documentation distinguishes queueing from steering; Bakery should expose whichever semantics its runtime actually supports, with a visible receipt, rather than making Enter ambiguous.[^cursor-overview]

The compact change summary above the composer should show real added/deleted counts and a “See diff” action. That action opens the existing Review panel alongside the conversation. It must identify whether the count describes proposals, working-tree changes, or the task baseline comparison.

## Review is a revision-specific decision

Use the supplied GitHub-style reference for the central interaction: changed-file navigation, side-by-side original and revised content, line numbers, clear additions/deletions, and a compact review action area. Reuse Monaco's diff editor and Bakery's current review plumbing.

Keep three distinctions explicit:

| Object | What the human is assessing | Required wording |
|---|---|---|
| Proposed edit | A requested change not yet applied | Accept/reject proposal |
| Current task diff | Actual changes against the recorded baseline | Review changes |
| Reviewed revision | The exact state approved by the human | Sign off this revision |

Tests and checks should show the command, outcome, and the revision or run they establish. Previously successful tests are not automatically proof for a later changed tree. If a diff changes during review, expose staleness and require a fresh comparison before approval, retaining the server-side check already reported in this implementation.

Untracked files, unreadable files, binary files, renames, and omitted large content need explicit representations. A refused read is not an unchanged file. No cosmetic green check should conceal a partial review. Signing off is not equivalent to pushing a PR or merging a dependency chain.

## Make configuration understandable without reducing capability

The project configuration work should stay, but its placement and labels should reflect the user's goal. Most people create a project before choosing a version manager, task runner, container runtime, or secret backend. Progressive disclosure is useful for this secondary detail, provided essential choices and status remain visible.[^primer-disclosure]

| Scope | Belongs here | Suggested language |
|---|---|---|
| Factory | Accounts, nodes, default storage, installed integrations | Provider accounts; Nodes; Storage; Integrations |
| Project | Roots, languages/runtimes, task definitions, service connections | Folders; Runtime versions; Build commands; Services |
| Socket | Active roots, environment overrides, terminals, task binding | Session environment; Terminals |
| Agent | Role/instructions, default account/model, permitted tools | Agent configuration |
| Turn | Model/effort override, attachments, current execution mode | Composer controls |

Show product names such as mise, proto, moon, or container adapters inside their relevant configuration sections, with detection and an explanation. Moon's project configuration is a reference for how project-level options and tasks can be represented, not a reason to require Moon or a manifest before chatting.[^moon]

Do not add an ever-growing hardcoded dropdown of tools simply to claim broad support. Derive available adapters from actual integrations and distinguish configured, installed, available, and unsupported. Preserve a custom-command or equivalent extension path where the product actually supports one. Future hosting integrations should become scoped operations and resource views, not force a redesign of project identity.

## Design-system implementation choice

The smallest practical choice is **existing semantic CSS tokens plus a small owned Svelte component layer**, using Bits UI where robust headless interaction is needed. Bits UI documents unstyled Svelte primitives and leaves styling to the application. That fits Bakery's current CSS-based stack.[^bits]

Shadcn-svelte is a valid alternative if the team deliberately wants its source-owned component workflow and Tailwind conventions. It is not necessary to install Tailwind to make this application beautiful. Its documentation identifies Bits UI and Tailwind as core foundations; adopting it should be one conscious system choice, not a second styling system layered over the first.[^shadcn]

| Approach | Recommendation for this pass |
|---|---|
| Bits UI + existing tokens | Preferred for menus, popovers, dialogs, selections, and focus behavior |
| Shadcn-svelte | Consider only with an explicit decision about Tailwind and component ownership |
| Primer / Carbon | Reuse behavioral and design guidance; do not import a competing application shell |
| New editor or dock engine | Not justified by this research; keep Monaco and Dockview |
| Full foreign IDE fork | Separate strategic decision, not a prerequisite for this coherence pass |

Own a small set of reusable primitives: button, icon button, menu, popover, dialog, select, row, status indicator, empty/error state, panel header, and composer controls. Centralize behavior and token use; do not rebuild mature editor, terminal, or docking internals. Library accessibility support still needs integration testing in Bakery.

### Proposed visual defaults

These are starting values for Bakery prototypes, not measurements copied from reference screenshots:

- Interface text: 13–14 px; secondary metadata generally 12 px; conversation text 15–16 px; code independently configurable.
- Compact desktop controls: roughly 28–32 px high, with adequately spaced targets and larger touch variants.
- Sidebar: roughly 240–280 px, resizable; compact rows with a second line only when it adds information.
- Spacing: a small 4/8/12/16/24 px scale; radii around 4/6/8 px by component purpose.
- Neutral surfaces, one main accent, and stable semantic status colors. Borders and alignment should do more work than shadows.
- Motion reserved for feedback and transitions. Avoid continuously glowing or pulsing every running task; respect reduced-motion preferences.

Density should come from fewer redundant wrappers and labels, not tiny text. For example, the current [TaskRow](../../bakery-client/src/lib/shell/TaskRow.svelte) uses an 8.5 px model chip. That is a concrete candidate for simplification and legibility improvement, not a reason to make the whole Floor spacious.

Use one icon vocabulary. Give icon-only controls accessible names and explanatory tooltips. A chevron, overflow menu, close button, and disclosure control should not look interchangeable. Splitters need keyboard operation, labels, and exposed values as well as draggable hit areas; the WAI-ARIA splitter pattern provides relevant guidance.[^splitter]

## Prove the experience, not just the build

The acceptance fixture should cover the actual task journey: ask Baker for a small bounded change, inspect the created task, enter its studio, steer it, observe proof execution, review the diff, and sign off. Run it in the isolated test environment without stopping the operator's live node. Then perform the corresponding supervised real-account Tauri smoke.

For the design pass, exercise at least these states:

| Scenario | What must remain true |
|---|---|
| New rootless project | Chat works; file area offers a truthful path to adding a root |
| Empty directory project | Root is clear; create-file and create-folder actions are reachable |
| Five concurrent tasks | Activity is readable; entering one does not stop the others |
| Model/account unavailable | History remains visible; recovery action is specific |
| Delayed or failed refresh | Counts and selection do not silently switch to guessed data |
| Review changes while open | Staleness is visible; sign-off checks the new state |
| Close/reopen and reload | Drafts, agent binding, and supported layout state survive |
| Narrow window and keyboard use | Controls remain reachable without overlapping the transcript |
| Large history and project list | Incremental updates avoid re-rendering every transcript/tree |

Measure responsiveness on the actual Mac. A reasonable initial target is visible local acknowledgement within about 100 ms for selection, opening a menu, or accepting a click; this is a proposed engineering target, not a claim about current performance. Network work can finish later while selection, drafts, and status remain responsive.

A screenshot review can catch alignment and density problems, but it cannot prove lifecycle behavior. Browser tests can catch live-state issues, but Chromium alone cannot prove native WKWebView docking. Keep both checks, scoped to the changed experience, instead of repeatedly running every expensive suite during CSS iteration.

## Delivery order

Do not start another broad infrastructure rewrite. Use a short sequence with explicit acceptance:

1. **Close the acceptance gaps in Baker Control.** Address the direct control-API bypass, add the task/review browser journey, and run the supervised actual-account checkpoint. Keep this separate from visual work so a design pass does not obscure runtime changes.
2. **Approve three concrete compositions.** Factory Floor with five tasks; permanent Baker with a work-order inspector; project Studio with composer, Code, Review, and terminals. Use representative real states, not placeholder metrics. Get the operator's approval on navigation and overflow-menu locations before widespread implementation.
3. **Build the shared component layer and apply it incrementally.** Reuse the current theme source. Start with rows, headers, menus, and empty states; then the shared composer. Preserve IDs, provider contracts, and user-owned layouts.
4. **Run the coherence work through Baker.** The existing A6 workstreams are an appropriate first real job once Checkpoint 1 is accepted. Give shared theme/components a clear owner; parallel workers should have bounded file ownership and an explicit integration order.
5. **Complete multi-task integration before expanding autonomy.** The ancestry and shipping checkpoint matters for dependent work. Plain-directory task execution needs its own truthful materialization support. Multi-node and phone access should extend the same authenticated state and action model, not create a second management application.

Memory, wiki, and reusable learning remain important. They should attach to durable projects, agents, artifacts, and provenance. The current design should leave clear places to inspect context and knowledge, without suggesting that a “Memory” panel alone implements validated learning. Hermes and DeepSeek are architectural reading for that later work, not evidence that it is already present.[^hermes][^deepseek]

### Suggested first design work order for Baker

> Improve the Factory Floor, permanent Baker, and project Studio using this research brief and the existing Studio contract. Preserve runtime behavior and durable identities. Reuse Monaco, Dockview, xterm, and the central theme. First present the three representative compositions for approval. Then implement shared controls and the approved views with bounded ownership. Do not replace panes, reset saved layouts, add fake counters, advertise unimplemented commands, or hide history when an account is unavailable. Demonstrate creation, steering, review, sign-off, close/reopen, and reload in the real desktop application.

The next operator input should be approval of those concrete compositions, not a choice between a dozen frontend libraries. The successful result is the operator using Baker to run work and entering any task to steer and assess it, with an interface that makes those actions routine.

## Sources

Official living documentation below was consulted on 9 September 2026. Publication dates are given when identifiable; otherwise the access date describes the research snapshot. Local source links describe the working tree at that time and may change.

[^zed]: Zed Industries, [Parallel Agents](https://zed.dev/docs/ai/parallel-agents), official product documentation, accessed 2026-09-09.
[^cursor]: Cursor, [Agents Window](https://cursor.com/docs/agent/agents-window), official product documentation, accessed 2026-09-09.
[^vscode]: Microsoft, [Agents window (Preview)](https://code.visualstudio.com/docs/agents/run/agents-window), official VS Code documentation, accessed 2026-09-09. Preview status is material to the comparison.
[^omp]: oh-my-pi maintainers, [Agent Hub](https://github.com/can1357/oh-my-pi/blob/main/docs/agent-hub.md), first-party repository documentation, accessed 2026-09-09.
[^linear]: Karri Saarinen and the Linear team, [How we redesigned the Linear UI (part Ⅱ)](https://linear.app/now/how-we-redesigned-the-linear-ui), 2024-03-28. Used for the documented design approach, not a claim about every current Linear screen.
[^primer-tokens]: GitHub Primer, [Token names](https://primer.style/product/primitives/token-names/), official product design-system documentation, accessed 2026-09-09.
[^carbon]: IBM Carbon Design System, [Data table: Usage](https://carbondesignsystem.com/components/data-table/usage/), official design-system documentation, accessed 2026-09-09.
[^hermes]: Nous Research, [Hermes Agent](https://github.com/NousResearch/hermes-agent), first-party README, accessed 2026-09-09. Capability descriptions are project documentation, not independently benchmarked results.
[^deepseek]: DeepSeek, [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness), first-party README, accessed 2026-09-09; local `docs/user/guide/index.md` also inspected. The project identifies itself as developer preview.
[^dockpanel]: DockPanel, [Project README](https://github.com/ovexro/dockpanel), first-party repository documentation, accessed 2026-09-09. Used as an operations-product reference, not a measured performance comparison.
[^moon]: moonrepo, [Project configuration](https://moonrepo.dev/docs/config/project), official documentation, accessed 2026-09-09.
[^refero]: [Refero](https://refero.design/), operator-requested inspiration library. Specific screens were not inspected; no design claim in this brief relies on its unseen contents.
[^primer-empty]: GitHub Primer, [Empty states](https://primer.style/product/ui-patterns/empty-states/), official product design-system guidance, accessed 2026-09-09.
[^primer-disclosure]: GitHub Primer, [Progressive disclosure](https://primer.style/product/ui-patterns/progressive-disclosure/), official product design-system guidance, accessed 2026-09-09.
[^dockview]: Dockview, [Rendering](https://dockview.dev/docs/core/panels/rendering/), official documentation, accessed 2026-09-09. Check APIs against Bakery's installed version before implementation.
[^hai]: Microsoft Research, [How to build effective human-AI interaction: Considerations for machine learning and software engineering](https://www.microsoft.com/en-us/research/articles/how-to-build-effective-human-ai-interaction-considerations-for-machine-learning-and-software-engineering/), first-party research guidance, accessed 2026-09-09.
[^cursor-overview]: Cursor, [Agent overview](https://cursor.com/docs/agent/overview), official product documentation, accessed 2026-09-09.
[^bits]: Bits UI, [Introduction](https://www.bits-ui.com/docs/introduction), official Svelte component documentation, accessed 2026-09-09.
[^shadcn]: shadcn-svelte, [Introduction](https://www.shadcn-svelte.com/docs), official documentation, accessed 2026-09-09.
[^splitter]: W3C WAI-ARIA Authoring Practices Guide, [Window Splitter Pattern](https://www.w3.org/WAI/ARIA/apg/patterns/windowsplitter/), accessed 2026-09-09. Design guidance is not a certification of Bakery's implementation.

<script lang="ts">
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import { toHandle } from "$lib/handle";
  import {
    node,
    defaultProjectStacks,
    type CreateProjectRootArgs,
    type InspectResp,
    type ProjectStacks,
    type StreamRef,
  } from "$lib/node";
  import { studioStore } from "$lib/studioStore.svelte";
  import FactoryFloor from "$lib/controlplane/FactoryFloor.svelte";
  import WorkBoard from "$lib/shell/WorkBoard.svelte";
  import ProjectEnvironment from "$lib/controlplane/ProjectEnvironment.svelte";
  import { socketLabel, isLegacyBakeryBranch, socketTag } from "$lib/sessionGroups";
  import { allConversations, STATUSES } from "$lib/conversations";
  import { pickFolder } from "$lib/desktop";

  // 07d-A2 Factory Floor: the one landing page. Owns the single page header
  // (no duplicate title in the embedded FactoryFloor block below). Merges
  // the old root foyer's open/create/attach wizard with the live-status
  // dashboard that used to live at /w, plus Nodes & streams and Knowledge
  // Heart as ordinary Floor content (07d-A2 decision — no separate
  // ?panel=nodes|heart route anymore). Retired sessions and a per-project
  // detail (sidebar §2 "Clicks": "project name → Floor project detail") are
  // both Floor views (?view=retired, ?project=<project_id> — 07d-A3: a
  // server-minted Project id now, never a repo_root path) reusing this same
  // page — never a separate app/route.
  //
  // The old wizard's second "Sessions" card grid is dropped here — the
  // permanent sidebar's Projects tree and this page's own "Recent sessions"
  // card (inside FactoryFloor) already cover that; a third listing was the
  // exact sprawl this slice exists to remove.

  const retiredView = $derived(page.url.searchParams.get("view") === "retired");
  /** `?view=archived`: every archived chat and thread, restorable from ⋯. */
  const archivedView = $derived(page.url.searchParams.get("view") === "archived");
  /** `?open=project` (or `create`) shows the open/create wizards. */
  const opener = $derived(page.url.searchParams.get("open"));

  // Every conversation's status comes from the node; the board (AI Elements)
  // lists them, and each project card counts its own.
  const convs = $derived(allConversations(studioStore.data));
  const projectCards = $derived(
    [...(studioStore.data?.projects ?? [])]
      .sort((a, b) => Number(b.pinned) - Number(a.pinned))
      .map((g) => {
        const ids = new Set(g.sockets.map((s) => s.stream_id));
        const mine = convs.filter((c) => ids.has(c.streamId) && !c.archived);
        const counts = STATUSES.map((s) => ({ ...s, n: mine.filter((c) => c.status === s.status).length })).filter(
          (s) => s.n > 0,
        );
        return { g, total: mine.length, counts };
      }),
  );
  const archivedCount = $derived(convs.filter((c) => c.archived).length);

  const retiredSockets = $derived(studioStore.sockets.filter((s) => s.discarded));

  // 07d-A3: grouping is by real Project membership straight off
  // `/studio/state`'s own `projects` bucket — never a client-side repo_root
  // match. A project_id that no longer resolves (bad link, or the Project
  // was since retired) shows as not found rather than silently empty.
  const projectParam = $derived(page.url.searchParams.get("project"));
  const projectGroup = $derived(studioStore.data?.projects.find((p) => p.project_id === projectParam) ?? null);
  const projectDetailName = $derived(projectGroup?.name ?? "");

  function retiredSecondary(s: StreamRef): string {
    const parts: string[] = [s.project || s.repo_root];
    if (socketLabel(s) !== s.branch && s.branch) parts.push(s.branch);
    return parts.join(" · ");
  }


  // ---- open wizard ----------------------------------------------------------
  let pickedPath = $state("");
  let inspected = $state<InspectResp | null>(null);
  let wizName = $state("");
  let branchMode = $state<"new" | "existing">("new");
  let wizBranch = $state("");
  let wizBase = $state("");
  let wizNewBranch = $state("");
  let busy = $state(false);
  let notice = $state("");
  let canBrowse = $state(true);

  let newWorktreePreview = $derived.by(() => {
    const p = inspected?.project;
    if (!p?.is_git || !p.root || !inspected) return "";
    const name = wizName.trim() || "…";
    const branch =
      (branchMode === "existing" ? wizBranch : wizNewBranch.trim()) ||
      inspected.suggested_branch ||
      "…";
    return `Will create worktree at ${inspected.worktree_base}/${name} from source ${p.root} on branch ${branch}`;
  });

  function shortPath(p: string) {
    if (!p) return "—";
    const parts = p.split("/");
    return parts.length > 3 ? "…/" + parts.slice(-3).join("/") : p;
  }

  async function browse() {
    try {
      const picked = await pickFolder("Choose a project folder");
      if (picked) {
        pickedPath = picked;
        await inspectPath();
      }
    } catch {
      canBrowse = false;
      notice = "Folder picker needs the desktop app — type or paste the path instead.";
    }
  }

  async function inspectPath() {
    notice = "";
    inspected = null;
    if (!pickedPath.trim()) return;
    busy = true;
    try {
      inspected = await node.inspect(pickedPath.trim());
      wizName = inspected.suggested_name;
      wizBase = inspected.project.default_branch ?? "";
      wizBranch = inspected.project.default_branch ?? "";
      wizNewBranch = inspected.suggested_branch;
      branchMode = "new";
    } catch (e) {
      notice = `inspect failed: ${e}`;
    } finally {
      busy = false;
    }
  }

  async function openWorktree() {
    if (!inspected?.project.root) return;
    await doOpen({
      repo: inspected.project.root,
      name: wizName,
      branch: branchMode === "existing" ? wizBranch : wizNewBranch.trim() || undefined,
      base_branch: branchMode === "new" ? wizBase : undefined,
    });
  }

  async function attachWorktree(path: string, name: string) {
    if (!inspected?.project.root) return;
    await doOpen({
      repo: inspected.project.root,
      name,
      worktree_path: path,
    });
  }

  async function reopen(s: StreamRef) {
    await doOpen({ repo: s.repo_root, name: s.name });
  }

  async function doOpen(args: Parameters<typeof node.open>[0]) {
    notice = "";
    busy = true;
    try {
      const res = await node.open(args);
      await goto(`/w/${toHandle(res.context.stream_id)}`);
    } catch (e) {
      notice = `${e}`;
    } finally {
      busy = false;
    }
  }

  // ---- create project (07d-A3 §0/§1: opening or creating a project must
  // never require Git) ---------------------------------------------------
  let newProjName = $state("");
  let newProjRootMode = $state<"bakery" | "existing" | "create">("bakery");
  /** "existing" mode only — a single freeform path, unchanged from before. */
  let newProjPath = $state("");
  /** "create" mode only — an explicit parent + editable child-folder name,
   *  replacing the old single freeform path (07d follow-up: a "create a new
   *  folder" request against an already-existing path used to silently
   *  attach to it instead of erroring). */
  let newProjParent = $state("");
  let newProjChildName = $state("");
  let newProjBusy = $state(false);
  let newProjNotice = $state("");
  let newProjCanBrowse = $state(true);
  let newProjStacks = $state<ProjectStacks>(defaultProjectStacks());

  async function browseForDirectory(assign: (path: string) => void, title: string) {
    try {
      const picked = await pickFolder(title);
      if (picked) assign(picked);
    } catch {
      newProjCanBrowse = false;
      newProjNotice = "Folder picker needs the desktop app — type or paste the path instead.";
    }
  }

  function isValidFolderName(name: string): boolean {
    const n = name.trim();
    return n.length > 0 && n !== "." && n !== ".." && !n.includes("/") && !n.includes("\\");
  }

  async function createProject() {
    newProjNotice = "";
    const name = newProjName.trim();
    if (!name) return;
    if (newProjRootMode === "existing" && !newProjPath.trim()) return;
    if (newProjRootMode === "create" && (!newProjParent.trim() || !isValidFolderName(newProjChildName))) return;
    newProjBusy = true;
    try {
      const root: CreateProjectRootArgs =
        newProjRootMode === "bakery"
          ? { mode: "bakery" }
          : newProjRootMode === "existing"
            ? { mode: "attach", path: newProjPath.trim() }
            : { mode: "create", parent: newProjParent.trim(), name: newProjChildName.trim() };
      const resp = await node.projectCreate(name, root, newProjStacks);
      studioStore.refresh();
      // A new project opens as its workspace: the conversation beside its files.
      await goto(`/w/${toHandle(resp.socket.stream_id)}`);
    } catch (e) {
      newProjNotice = `${e}`;
    } finally {
      newProjBusy = false;
    }
  }

  // The one-step path: a name, Create — Bakery makes the folder inside its
  // own. A folder you already have, and everything else, is under More options.
  async function quickCreate() {
    newProjRootMode = "bakery";
    await createProject();
  }

  onMount(() => {
    studioStore.ensureStarted();
  });
</script>

<div class="page">
  <header class="page-hd">
    <div class="hd-row">
      <h1>
        {projectParam
          ? projectDetailName || "Project not found"
          : archivedView
            ? "Archived"
            : retiredView
              ? "Retired"
              : "Factory floor"}
      </h1>
      <span class="grow"></span>
      {#if retiredView || archivedView || projectParam}
        <a class="back-link" href="/">‹ Back to the floor</a>
      {:else}
        <!-- New chat and new thread are the sidebar's; this opens the form below. -->
        <a class="btn ghost sm" href={opener ? "/" : "/?open=project"} aria-expanded={!!opener}>
          {opener ? "Close" : "New project"}
        </a>
      {/if}
    </div>
    <!-- Only a fact the title does not already say: where it lives. -->
    {#if projectGroup}
      <p class="lede">
        {projectGroup.roots.length ? projectGroup.roots.map((r) => r.path).join(" · ") : "No folder"}
      </p>
    {/if}
  </header>

  {#if retiredView}
    <div class="retired-list">
      {#if retiredSockets.length === 0}
        <p class="muted">No retired sessions.</p>
      {:else}
        {#each retiredSockets as s (s.stream_id)}
          <div class="retired-row">
            <span class="retired-name">{socketLabel(s)}</span>
            <span class="retired-secondary">{retiredSecondary(s)}</span>
            {#if isLegacyBakeryBranch(s.branch)}<span class="badge">legacy</span>{/if}
            {#if socketTag(s) === "in place"}<span class="badge">in place</span>{/if}
          </div>
        {/each}
      {/if}
    </div>
  {:else if archivedView}
    <WorkBoard scope={{ kind: "archived" }} emptyText="Nothing archived." />
  {:else if projectParam}
    <!-- Project detail: its chats and tasks by status, on the same board. -->
    {#if projectGroup}
      {#key projectGroup.project_id}
        <WorkBoard
          scope={{ kind: "project", projectId: projectGroup.project_id }}
          emptyText="No active chats or tasks in this project."
        />
        <ProjectEnvironment projectId={projectGroup.project_id} />
      {/key}
    {/if}
  {:else}
    {#if opener}
    <section class="panel new-project" aria-label="New project" data-testid="new-project">
      <div class="panel-hd">
        <h2>New project</h2>
      </div>
      <div class="panel-body">
        <div class="row">
          <input
            placeholder="Project name"
            bind:value={newProjName}
            aria-label="Project name"
            onkeydown={(e) => {
              if (e.key === "Enter" && newProjName.trim()) void quickCreate();
            }}
          />
          <button
            type="button"
            class="btn"
            disabled={!newProjName.trim() || newProjBusy}
            onclick={() => void quickCreate()}
            data-testid="new-project-create"
          >{newProjBusy ? "Creating…" : "Create project"}</button>
        </div>
        {#if newProjNotice}<p class="notice bad">{newProjNotice}</p>{/if}
        <details class="more-options">
          <summary>More options</summary>
    <div class="project-actions">
      <section class="panel wizard create-project">
        <div class="panel-hd">
          <h2>Folder and toolchains</h2>
        </div>
        <div class="panel-body">
        <div class="row">
          <input placeholder="project name" bind:value={newProjName} />
        </div>
        <div class="row">
          <label class="radio">
            <input type="radio" bind:group={newProjRootMode} value="bakery" /> a new folder inside Bakery
          </label>
          <label class="radio">
            <input type="radio" bind:group={newProjRootMode} value="existing" /> use an existing folder
          </label>
          <label class="radio">
            <input type="radio" bind:group={newProjRootMode} value="create" /> a new folder somewhere else
          </label>
        </div>
        {#if newProjRootMode === "existing"}
          <div class="row">
            <input placeholder="folder path" bind:value={newProjPath} />
            {#if newProjCanBrowse}
              <button
                type="button"
                class="btn ghost"
                onclick={() => browseForDirectory((p) => (newProjPath = p), "Choose a project folder")}
              >Browse…</button>
            {/if}
          </div>
        {:else if newProjRootMode === "create"}
          <div class="row">
            <input placeholder="parent folder" bind:value={newProjParent} />
            {#if newProjCanBrowse}
              <button
                type="button"
                class="btn ghost"
                onclick={() => browseForDirectory((p) => (newProjParent = p), "Choose a parent folder")}
              >Browse…</button>
            {/if}
          </div>
          <div class="row">
            <input placeholder="new folder name" bind:value={newProjChildName} />
          </div>
        {/if}
        <div class="row stacks">
          <label>Toolchain
            <select bind:value={newProjStacks.toolchain}>
              <option value="none">none (thin room)</option>
              <option value="mise">mise</option>
              <option value="proto">proto (moonrepo)</option>
            </select>
          </label>
          <label>Tasks
            <select bind:value={newProjStacks.tasks}>
              <option value="none">none</option>
              <option value="moon">moon</option>
            </select>
          </label>
          <label>Containers
            <select bind:value={newProjStacks.containers}>
              <option value="none">none</option>
              <option value="colima">colima</option>
              <option value="orbstack">orbstack</option>
            </select>
          </label>
          <label>Secrets
            <select bind:value={newProjStacks.secrets}>
              <option value="bakery">bakery</option>
              <option value="infisical">infisical</option>
            </select>
          </label>
        </div>
        <div class="row">
          <button
            type="button"
            class="btn"
            disabled={
              !newProjName.trim() ||
              (newProjRootMode === "existing" && !newProjPath.trim()) ||
              (newProjRootMode === "create" && (!newProjParent.trim() || !isValidFolderName(newProjChildName))) ||
              newProjBusy
            }
            onclick={createProject}
          >
            Create project
          </button>
        </div>
        {#if newProjNotice}<p class="notice bad">{newProjNotice}</p>{/if}
        </div>
      </section>

      <section class="panel wizard open-project">
        <div class="panel-hd">
          <h2>Git worktree</h2>
        </div>
        <div class="panel-body">
        <div class="row">
          <input placeholder="project folder (any git repo)" bind:value={pickedPath} />
          {#if canBrowse}
            <button type="button" class="btn ghost" onclick={browse}>Browse…</button>
          {/if}
          <button type="button" class="btn" onclick={inspectPath} disabled={!pickedPath.trim() || busy}>
            Inspect
          </button>
        </div>

        {#if inspected && !inspected.project.is_git}
          <p class="notice bad">Not a git repository: {inspected.project.error}</p>
        {/if}

        {#if inspected?.project.is_git}
          {@const p = inspected.project}
          <div class="repo-facts">
            <strong>{p.name}</strong>
            <code class="path" title={p.root ?? ""}>{shortPath(p.root ?? "")}</code>
            <span class="badge">default: {p.default_branch ?? "?"}</span>
            <span class="badge">{p.local_branches.length} branches</span>
            <span class="badge">{p.worktrees.length} worktrees</span>
          </div>

          {#if inspected.bakery_streams.length}
            <div class="group">
              <h3>Already onboarded here <span class="count-chip">{inspected.bakery_streams.length}</span></h3>
              <ul class="picks">
                {#each inspected.bakery_streams as s (s.stream_id)}
                  <li>
                    <strong>{s.project || s.name}</strong>
                    <span class="branch">{s.branch}</span>
                    {#if s.in_place}<span class="badge">in place</span>{/if}
                    {#if !s.exists_on_disk}<span class="badge warn">missing on disk</span>{/if}
                    <code class="path muted" title={s.repo_root}>{shortPath(s.repo_root)}</code>
                    <button type="button" class="btn ghost sm" disabled={busy} onclick={() => reopen(s)}>
                      Re-open
                    </button>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}

          {#if p.root}
            {@const root = p.root}
            {@const mainBranch = p.worktrees.find((w) => w.is_main)?.branch ?? p.current_branch}
            {@const inPlaceName = inspected.suggested_name}
            <div class="group inplace">
              <h3>Open in place</h3>
              <p class="preview">
                Edits apply directly to <code class="path" title={root}>{root}</code> on
                <span class="branch">{mainBranch ?? "(detached)"}</span>.
              </p>
              <button
                type="button"
                class="btn ghost sm"
                disabled={busy || !mainBranch}
                onclick={() => attachWorktree(root, inPlaceName)}
              >
                Open in place
              </button>
            </div>
          {/if}

          {#if p.worktrees.filter((w) => !w.is_main).length}
            <div class="group">
              <h3>Existing linked worktrees</h3>
              <ul class="picks">
                {#each p.worktrees.filter((w) => !w.is_main) as w (w.path)}
                  <li>
                    <code class="path" title={w.path}>{shortPath(w.path)}</code>
                    <span class="branch">{w.branch ?? "(detached)"}</span>
                    <button
                      type="button"
                      class="btn ghost sm"
                      disabled={busy || !w.branch}
                      onclick={() => attachWorktree(w.path, w.path.split("/").pop() || w.branch!)}
                    >
                      Attach
                    </button>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}

          <div class="group">
            <h3>New worktree</h3>
            <div class="row">
              <input placeholder="worktree name" bind:value={wizName} />
              <label class="radio">
                <input type="radio" bind:group={branchMode} value="new" /> new branch
              </label>
              <label class="radio">
                <input type="radio" bind:group={branchMode} value="existing" /> existing branch
              </label>
            </div>
            <div class="row">
              {#if branchMode === "new"}
                <span class="muted">branch</span>
                <input placeholder="new branch name" bind:value={wizNewBranch} />
                <span class="muted">from</span>
                <select bind:value={wizBase}>
                  {#each p.local_branches as b}<option value={b}>{b}</option>{/each}
                </select>
              {:else}
                <span class="muted">check out</span>
                <select bind:value={wizBranch}>
                  {#each p.local_branches as b}<option value={b}>{b}</option>{/each}
                </select>
              {/if}
              <button type="button" class="btn" disabled={!wizName.trim() || busy} onclick={openWorktree}>
                Open worktree
              </button>
            </div>
            <p class="preview">{newWorktreePreview}</p>
          </div>
        {/if}

        {#if notice}<p class="notice">{notice}</p>{/if}
        </div>
      </section>
    </div>
        </details>
      </div>
    </section>
    {/if}

    <section class="fsec" aria-label="Work">
      <div class="fsec-hd">
        <h2>Work</h2>
      </div>
      <WorkBoard scope={{ kind: "all" }} emptyText="Nothing yet." />
    </section>

    <section class="fsec" aria-label="Projects">
      <div class="fsec-hd">
        <h2>Projects</h2>
        <span class="count-chip">{projectCards.length}</span>
      </div>
      {#if projectCards.length === 0}
        <p class="muted">No projects yet.</p>
      {/if}
      <div class="cards">
        {#each projectCards as { g, total, counts } (g.project_id)}
          <a class="card project" href="/?project={encodeURIComponent(g.project_id)}">
            <div class="card-hd">
              <span class="card-title">{g.name}</span>
              {#if g.pinned}<span class="badge">pinned</span>{/if}
              <span class="grow"></span>
              <span class="card-meta">{total} active</span>
            </div>
            <p class="card-roots">{g.roots.length ? g.roots.map((r) => r.path).join(" · ") : "No folder"}</p>
            {#if counts.length > 0}
              <div class="card-counts">
                {#each counts as c (c.status)}
                  <span class="count {c.status}">{c.n} {c.short}</span>
                {/each}
              </div>
            {/if}
          </a>
        {/each}
      </div>
    </section>

    {#if archivedCount > 0}
      <p class="muted"><a href="/?view=archived">{archivedCount} archived</a></p>
    {/if}

    <section class="floor-block">
      <FactoryFloor />
    </section>

  {/if}
</div>

<style>
  .page {
    height: 100%;
    width: 100%;
    overflow-y: auto;
    overflow-x: hidden;
    box-sizing: border-box;
    padding: 18px 24px 40px;
    max-width: 1240px;
    margin: 0 auto;
    container-type: inline-size;
  }

  .page-hd {
    margin-bottom: 16px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .hd-row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    min-width: 0;
  }
  .page-hd h1 {
    margin: 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 600 17px/1.3 var(--font-ui);
    letter-spacing: -0.01em;
    color: var(--fg);
  }
  .lede {
    margin: 0;
    font: 400 11.5px/1.4 var(--font-mono);
    color: var(--fg-faint);
    overflow-wrap: anywhere;
  }
  .back-link {
    display: inline-flex;
    align-items: center;
    min-height: 28px;
    padding: 0 10px;
    border-radius: var(--r-lg);
    font: 500 12px/1 var(--font-ui);
    color: var(--accent-fg);
    text-decoration: none;
  }
  .back-link:hover { background: var(--accent-bg); }

  .floor-block {
    margin-top: 20px;
    padding-top: 16px;
    border-top: 1px solid var(--line-chrome);
    scroll-margin-top: 20px;
  }

  a.btn { text-decoration: none; }

  /* ---- overview sections ---- */

  .fsec { margin-top: 18px; }
  .fsec-hd {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
  }
  .fsec-hd h2 {
    margin: 0;
    font: 600 12.5px/1.2 var(--font-ui);
    color: var(--fg-2);
  }
  .grow { flex: 1 1 auto; }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 300px), 1fr));
    gap: 8px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    padding: 8px 10px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-xl);
    background: var(--bg-panel);
  }
  .card-hd {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .card-title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 600 13.5px/1.2 var(--font-ui);
    color: var(--fg);
    text-decoration: none;
  }
  .card-meta { flex: 0 0 auto; font: 400 11px/1 var(--font-ui); color: var(--fg-faint); }
  .card-roots {
    margin: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 400 10.5px/1.4 var(--font-mono);
    color: var(--fg-ghost);
  }

  a.card { text-decoration: none; color: inherit; }
  a.card:hover { border-color: var(--line-strong); }
  .card-counts { display: flex; flex-wrap: wrap; gap: 4px; padding-top: 2px; }
  .count {
    padding: 2px 8px;
    border-radius: var(--r-pill);
    background: var(--bg-chip);
    color: var(--fg-dim);
    font: 500 11px/1.4 var(--font-ui);
  }
  .count.needs_input { background: var(--warn-bg); color: var(--warn); }
  .count.failed { background: var(--bad-bg); color: var(--bad); }
  .count.working { background: var(--ok-bg); color: var(--ok); }
  .count.done { background: var(--accent-bg); color: var(--accent-fg); }

  /* ---- retired view ---- */

  .retired-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: 900px;
  }
  .retired-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding: 13px 15px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-2xl);
    background: var(--bg-panel);
  }
  .retired-name {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 600 13px/1.2 var(--font-ui);
    color: var(--fg);
  }
  .retired-secondary {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 400 11px/1.2 var(--font-mono);
    color: var(--fg-faint);
    margin-left: auto;
  }
  .retired-list > .muted {
    margin: 0;
    padding: 28px;
    border: 1px dashed var(--line-strong);
    border-radius: var(--r-2xl);
    background: var(--bg-panel);
    text-align: center;
    font: 400 12.5px/1.5 var(--font-ui);
  }

  /* ---- wizard (adapted from the old root foyer) ---- */

  .project-actions {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    align-items: start;
    gap: 18px;
  }
  .wizard { min-width: 0; }
  .new-project { margin-bottom: 8px; }
  .more-options > summary {
    cursor: pointer;
    font: 500 12px/1.4 var(--font-ui);
    color: var(--fg-dim);
  }
  .more-options[open] > summary { margin-bottom: 14px; }
  .panel {
    background: var(--bg-panel);
    border: 1px solid var(--line-soft);
    border-radius: var(--r-2xl);
    overflow: hidden;
  }
  .panel-hd {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 9px 14px;
    background: var(--bg-raised);
    border-bottom: 1px solid var(--line-soft);
  }
  .panel-hd h2 {
    margin: 0;
    font: 600 12.5px/1.3 var(--font-ui);
    color: var(--fg);
  }
  .panel-body {
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .count-chip {
    padding: 2px 5px;
    border-radius: var(--r-sm);
    background: var(--bg-chip);
    color: var(--fg-dim);
    font: 400 10.5px/1 var(--font-mono);
  }
  .row {
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
    min-width: 0;
  }
  .row input, .row select {
    background: var(--bg-app);
    border: 1px solid var(--line);
    border-radius: var(--r-xl);
    min-height: 32px;
    box-sizing: border-box;
    padding: 5px 10px;
    font: 400 12.5px/1.4 var(--font-ui);
    color: var(--fg);
    transition: border-color 120ms ease, box-shadow 120ms ease, background 120ms ease;
  }
  .row input::placeholder { color: var(--fg-fainter); }
  .row input:hover, .row select:hover { border-color: var(--line-strong); }
  .row input:focus-visible, .row select:focus-visible {
    outline: none;
    border-color: var(--accent-border);
    box-shadow: 0 0 0 3px var(--accent-bg);
  }
  .row input { flex: 1 1 220px; min-width: 0; }
  .row select { min-width: 0; }
  .row.stacks {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    align-items: stretch;
  }
  .row.stacks label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
    font: 500 10.5px/1 var(--font-ui);
    color: var(--fg-dim);
  }
  .row.stacks select { width: 100%; }

  .btn {
    min-height: 32px;
    padding: 6px 12px;
    border-radius: var(--r-xl);
    background: var(--accent-solid);
    border: 1px solid transparent;
    color: var(--fg-on-solid);
    font: 600 12px/1 var(--font-ui);
    cursor: pointer;
    white-space: nowrap;
    transition: background 120ms ease, border-color 120ms ease, transform 120ms ease;
  }
  .btn:hover:not(:disabled) { background: var(--accent-solid-h); }
  .btn:active:not(:disabled) { transform: translateY(1px); }
  .btn:focus-visible, .back-link:focus-visible {
    outline: 2px solid var(--accent-border);
    outline-offset: 2px;
  }
  .btn:disabled { opacity: 0.45; cursor: default; }

  .btn.ghost {
    background: none;
    border: 1px solid var(--line);
    color: var(--fg-2);
  }
  .btn.ghost:hover:not(:disabled) { background: var(--bg-chip); }
  .btn.sm { min-height: 26px; padding: 4px 10px; font-size: 11px; }

  .radio {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 30px;
    box-sizing: border-box;
    padding: 5px 9px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-lg);
    background: var(--bg-app);
    font: 400 11.5px/1.2 var(--font-ui);
    color: var(--fg-dim);
    cursor: pointer;
  }
  .radio:hover { border-color: var(--line-strong); color: var(--fg-2); }
  .radio:focus-within { border-color: var(--accent-border); box-shadow: 0 0 0 3px var(--accent-bg); }
  .radio input {
    flex: 0 0 13px;
    width: 13px;
    min-width: 13px;
    height: 13px;
    min-height: 13px;
    padding: 0;
    border: 0;
    background: transparent;
    box-shadow: none;
    accent-color: var(--accent-solid);
  }

  .muted { color: var(--fg-faint); }

  .badge {
    padding: 2px 6px;
    border-radius: var(--r-sm);
    background: var(--bg-chip);
    color: var(--fg-dim);
    font: 400 10.5px/1 var(--font-mono);
  }
  .badge.warn { background: var(--warn-bg); color: var(--warn); }

  .notice {
    margin: 0;
    font: 400 12px/1.4 var(--font-ui);
    color: var(--fg-dim);
  }
  .notice.bad { color: var(--bad); }

  .repo-facts {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
    align-items: center;
    min-width: 0;
  }
  .repo-facts strong { font: 600 13px/1 var(--font-ui); color: var(--fg); }

  .path {
    font: 400 11.5px/1 var(--font-mono);
    color: var(--fg-dim);
    overflow-wrap: anywhere;
  }

  .branch {
    font: 400 11px/1 var(--font-mono);
    color: var(--fg-faint);
  }

  .group {
    border-top: 1px solid var(--line-soft);
    padding-top: 14px;
  }
  .group h3 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 0 9px;
    font: 600 10.5px/1 var(--font-ui);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--fg-dim);
  }
  .group.inplace {
    background: var(--warn-bg);
    border: 1px solid var(--warn-border);
    border-radius: var(--r-2xl);
    border-top: 1px solid var(--warn-border);
    padding: 13px 14px;
  }

  .picks {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .picks li {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    flex-wrap: wrap;
    min-width: 0;
    padding: 9px 0;
    font: 400 12px/1.3 var(--font-ui);
    border-bottom: 1px solid var(--line-row);
  }
  .picks li:last-child { border-bottom: none; }
  .picks li button { margin-left: auto; }

  .preview {
    font: 400 11.5px/1.4 var(--font-ui);
    color: var(--fg-faint);
    margin: 6px 0 0;
    overflow-wrap: anywhere;
  }

  @media (max-width: 980px) {
    .project-actions { grid-template-columns: minmax(0, 1fr); }
  }

  @media (max-width: 720px) {
    .page { padding: 14px 14px 32px; }
    .retired-row { align-items: flex-start; flex-wrap: wrap; }
    .retired-secondary { order: 3; flex-basis: 100%; margin-left: 0; }
  }

  @media (max-width: 520px) {
    .row.stacks { grid-template-columns: minmax(0, 1fr); }
    .row > .btn:not(.sm) { flex: 1 1 auto; }
    .radio { flex: 1 1 100%; }
  }
</style>

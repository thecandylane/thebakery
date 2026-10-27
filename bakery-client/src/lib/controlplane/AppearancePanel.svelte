<script lang="ts">
  import { onMount, tick } from "svelte";
  import { beforeNavigate, goto } from "$app/navigation";
  import { ask } from "$lib/ask";
  import { guardSettingsClose } from "./SettingsFrame.svelte";
  import {
    AUTO_LABEL, BUILTIN_THEMES, THEME_LABELS, CUSTOM_TOKEN_KEYS, THEME_CHANGED_EVENT,
    loadAppearance, appearanceStorageIssue, exportAppearanceBackup, resetAppearanceLibrary,
    themeLabel, applyTheme, saveScheme, deleteScheme,
    previewTheme, restoreTheme, readPaletteTokens, isColor, colorToHex,
    accentTokens, exportScheme, importScheme,
    type AppearanceState, type BuiltinTheme, type Theme, type ColorScheme,
    type CustomTokenKey, type CustomTokens,
  } from "$lib/theme";
  import SchemePreview from "./SchemePreview.svelte";
  import { parseTweakcn, tweakcnScheme } from "$lib/tweakcn";
  import { BAKERY_THEMES, type BakeryTheme } from "$lib/bakeryThemes";

  type Draft = { id?: ColorScheme["id"]; name: string; base: BuiltinTheme; tokens: CustomTokens };
  const BLURBS: Record<BuiltinTheme, string> = {
    dark: "Bakery’s original dark canvas and blue accents.", dim: "Softer dark surfaces, with room to breathe.",
    light: "A bright workspace with crisp blue accents.", monokai: "Warm charcoal, vivid accents, familiar syntax.",
    "solarized-dark": "Deep teal with balanced, muted highlights.",
    "solarized-light": "Warm paper with the Solarized color family.",
    "high-contrast": "Strong separation and clear, bright details.",
    warm: "jcode's warm stone surfaces, ivory type, mono everywhere.",
    parchment: "jcode's light paper and ink, mono everywhere.",
  };
  const TOKEN_LABELS: Partial<Record<CustomTokenKey, string>> = {
    "--bg-rail": "Rail & terminal", "--bg-app": "Workspace", "--bg-chrome": "Sidebar & chrome",
    "--bg-panel": "Panels & composer", "--bg-raised": "Menus & raised surfaces", "--bg-chip": "Chips & hover",
    "--line-chrome": "Chrome dividers", "--line-row": "Row dividers", "--line-soft": "Subtle borders",
    "--line": "Control borders", "--line-strong": "Strong borders", "--scrollbar-thumb-hover": "Scrollbar hover",
    "--fg": "Primary text", "--fg-body": "Conversation text", "--fg-2": "Secondary text",
    "--fg-dim": "Metadata text", "--fg-faint": "Muted text", "--fg-fainter": "Placeholder text",
    "--fg-ghost": "Quiet text", "--fg-linenum": "Editor line numbers", "--fg-rule": "Text separators",
    "--fg-on-solid": "Text on buttons", "--fg-on-tint": "Text on bright tints",
    "--accent": "Accent color", "--accent-fg": "Accent text", "--accent-bg": "Accent tint",
    "--accent-bg-2": "Accent hover tint", "--accent-sel": "Selected rows", "--accent-solid-h": "Primary button hover",
    "--accent-border": "Accent border", "--accent-solid": "Primary buttons",
    "--ok": "Success", "--ok-bg": "Success tint", "--ok-border": "Success border",
    "--warn": "Warning", "--warn-bg": "Warning tint", "--warn-border": "Warning border",
    "--bad": "Error", "--bad-bg": "Error tint", "--bad-border": "Error border",
    "--violet": "Violet highlight", "--violet-bg": "Violet tint", "--idle": "Idle activity", "--gone-border": "Missing worktree border",
    "--baker-solid": "Baker buttons", "--baker-border": "Baker border", "--baker-solid-h": "Baker button hover",
    "--syntax-keyword": "Keywords", "--syntax-string": "Strings", "--syntax-comment": "Comments",
    "--syntax-number": "Numbers", "--syntax-type": "Types", "--overlay-scrim": "Modal background scrim",
    "--diff-add-bg": "Added line background", "--diff-del-bg": "Removed line background",
    "--diff-add-fg": "Added line text", "--diff-del-fg": "Removed line text",
    "--diff-add-bg-editor": "Editor added background", "--diff-del-bg-editor": "Editor removed background",
  };
  const GROUPS = [
    { label: "Surfaces", description: "Your canvas, sidebars, panels, and composer.", test: (key: string) => key.startsWith("--bg-") },
    { label: "Accent", description: "Selections, links, and primary controls.", test: (key: string) => key.startsWith("--accent") },
    { label: "Text", description: "From conversation text to quiet metadata.", test: (key: string) => key.startsWith("--fg") },
    { label: "Borders & overlays", description: "Dividers, scrollbars, and background scrims.", test: (key: string) => key.startsWith("--line") || key.startsWith("--scrollbar") || key.startsWith("--overlay") },
    { label: "Status", description: "Success, warning, error, and activity colors.", test: (key: string) => /^--(ok|warn|bad|gone|violet|idle)/.test(key) },
    { label: "Baker", description: "The permanent Baker’s own accent colors.", test: (key: string) => key.startsWith("--baker") },
    { label: "Review & diff", description: "Added and removed code in Review and the editor.", test: (key: string) => key.startsWith("--diff") },
    { label: "Code syntax", description: "Live syntax colors in open editors.", test: (key: string) => key.startsWith("--syntax") },
  ];
  const tokenGroups = [
    ...GROUPS.map((group) => ({ ...group, keys: CUSTOM_TOKEN_KEYS.filter(group.test) })),
    { label: "Additional colors", description: "Other shared Studio colors.", keys: CUSTOM_TOKEN_KEYS.filter((key) => !GROUPS.some((group) => group.test(key))) },
  ].filter((group) => group.keys.length > 0);

  let appearance = $state<AppearanceState>({ version: 1, selected: "auto", schemes: [] });
  let draft = $state<Draft | null>(null);
  let initialDraft = $state("");
  let baseTokens = $state<Partial<Record<CustomTokenKey, string>>>({});
  let invalidValues = $state<Partial<Record<CustomTokenKey, string>>>({});
  let livePreview = $state(true);
  let error = $state("");
  let storageIssue = $state<string | null>(null);
  let notice = $state("");
  let pendingDelete = $state<ColorScheme["id"] | null>(null);
  let nameInput = $state<HTMLInputElement>();
  let importInput = $state<HTMLInputElement>();
  let importing = $state(false);
  let confirmReset = $state(false);
  let importGeneration = 0;
  let alive = false;
  const dirty = $derived(draft !== null && (JSON.stringify(draft) !== initialDraft || Object.keys(invalidValues).length > 0));
  const invalidCount = $derived(Object.keys(invalidValues).length);
  const changedCount = $derived(Object.keys(draft?.tokens ?? {}).length);

  function explainError(cause: unknown) {
    error = cause instanceof Error ? cause.message : "The appearance change could not be saved. Please try again.";
  }

  // tweakcn: paste its CSS export, or a registry link that is fetched here.
  let tweakOpen = $state(false);
  let tweakText = $state("");
  let tweakName = $state("");
  let tweakBusy = $state(false);
  async function importTweak(event: SubmitEvent) {
    event.preventDefault();
    error = ""; notice = ""; tweakBusy = true;
    try {
      let text = tweakText.trim();
      if (/^https:\/\/\S+$/.test(text)) {
        const res = await fetch(text, { headers: { accept: "application/json" } });
        if (!res.ok) throw new Error(`That link answered ${res.status}; check it in tweakcn's Code → registry tab.`);
        text = await res.text();
      } else if (/^\w+:\/\//.test(text)) {
        throw new Error("Only https:// registry links can be fetched.");
      }
      const scheme = saveScheme(tweakcnScheme(parseTweakcn(text), tweakName));
      refresh();
      notice = `${scheme.name} is imported and applied${scheme.dark ? ", light and dark following your Mac" : ""}.`;
      tweakOpen = false; tweakText = ""; tweakName = "";
    } catch (cause) { explainError(cause); }
    finally { tweakBusy = false; }
  }
  // Bakery themes to try: one click imports one (once) and applies it.
  const bakerySwatches = (t: BakeryTheme) => {
    const parsed = parseTweakcn(t.css);
    const v = { ...parsed.theme, ...(parsed.dark ?? parsed.light ?? {}) };
    return [v.background, v.card, v.primary, v.foreground].filter(Boolean);
  };
  function tryBakeryTheme(t: BakeryTheme) {
    error = ""; notice = "";
    try {
      const saved = appearance.schemes.find((s) => s.name === t.name);
      if (saved) applyTheme(saved.id);
      else saveScheme(tweakcnScheme(parseTweakcn(t.css), t.name));
      refresh();
      notice = `${t.name} is applied. It's in Your schemes to keep, edit or delete.`;
    } catch (cause) { explainError(cause); }
  }
  function refresh() {
    appearance = loadAppearance(); storageIssue = appearanceStorageIssue();
    // A saved selection from the dial/another window supersedes this preview,
    // but never discards the draft. The toggle must report the actual mode.
    if (draft && !document.documentElement.hasAttribute("data-theme-preview")) livePreview = false;
  }
  function onThemeChanged(event: Event) {
    refresh();
    if (!(event as CustomEvent<{ preview?: boolean }>).detail?.preview) {
      importGeneration++; importing = false;
    }
  }
  onMount(() => {
    alive = true;
    refresh();
    window.addEventListener(THEME_CHANGED_EVENT, onThemeChanged);
    return () => {
      alive = false;
      importGeneration++;
      window.removeEventListener(THEME_CHANGED_EVENT, onThemeChanged);
      if (draft) restoreTheme();
    };
  });
  // A navigation can only be cancelled synchronously, but in the desktop app
  // the answer to a confirmation arrives later (see `ask`). So cancel first,
  // ask, and go again only on "yes".
  let discardConfirmed = false;
  // The Settings popup closes without navigating, so it asks here instead.
  $effect(() => {
    guardSettingsClose(dirty ? () => ask("Discard your unsaved color scheme changes?") : null);
    return () => guardSettingsClose(null);
  });
  beforeNavigate((navigation) => {
    if (!dirty || discardConfirmed) return;
    navigation.cancel();
    const target = navigation.to?.url;
    if (navigation.type === "leave" || !target) return;
    void ask("Discard your unsaved color scheme changes?").then((yes) => {
      if (!yes) return;
      discardConfirmed = true;
      void goto(target);
    });
  });
  function choose(id: Theme) {
    if (draft) return false;
    importGeneration++; importing = false;
    error = "";
    try { applyTheme(id); refresh(); notice = `${themeLabel(id, appearance)} is now applied throughout Studio.`; return true; }
    catch (cause) { explainError(cause); return false; }
  }
  function uniqueName(preferred: string) {
    let candidate = preferred;
    let suffix = 2;
    while (appearance.schemes.some((scheme) => scheme.name.toLocaleLowerCase() === candidate.toLocaleLowerCase())) candidate = `${preferred} ${suffix++}`;
    return candidate;
  }
  async function startEditing(scheme?: ColorScheme, duplicate = false) {
    if (draft) return;
    importGeneration++; importing = false;
    error = ""; notice = ""; pendingDelete = null;
    const active = appearance.schemes.find((item) => item.id === appearance.selected);
    const source = scheme ?? active;
    const base = source?.base ?? (BUILTIN_THEMES.includes(appearance.selected as BuiltinTheme) ? appearance.selected as BuiltinTheme : "dark");
    draft = {
      ...(scheme && !duplicate ? { id: scheme.id } : {}),
      name: scheme && !duplicate ? scheme.name : uniqueName(`${source?.name ?? THEME_LABELS[base]} copy`),
      base, tokens: { ...(source?.tokens ?? {}) },
    };
    initialDraft = scheme && !duplicate ? JSON.stringify(draft) : "";
    invalidValues = {}; baseTokens = readPaletteTokens(base); livePreview = true;
    renderPreview();
    await tick(); nameInput?.focus(); nameInput?.select();
  }
  function renderPreview() {
    if (!draft) return;
    try { if (livePreview) previewTheme(draft.base, draft.tokens); else restoreTheme(); }
    catch (cause) { explainError(cause); }
  }
  function changeBase(base: BuiltinTheme) {
    if (!draft) return;
    draft = { ...draft, base }; baseTokens = readPaletteTokens(base); renderPreview();
  }
  function tokenLabel(key: CustomTokenKey) {
    return TOKEN_LABELS[key] ?? key.slice(2).replace(/-/g, " ").replace(/^./, (letter) => letter.toUpperCase());
  }
  function tokenValue(key: CustomTokenKey): string { return invalidValues[key] ?? draft?.tokens[key] ?? baseTokens[key] ?? ""; }
  function updateToken(key: CustomTokenKey, value: string) {
    if (!draft) return;
    error = "";
    if (!isColor(value.trim())) { invalidValues = { ...invalidValues, [key]: value }; return; }
    const invalid = { ...invalidValues }; delete invalid[key]; invalidValues = invalid;
    draft = { ...draft, tokens: { ...draft.tokens, [key]: value.trim() } }; renderPreview();
  }
  function resetToken(key: CustomTokenKey) {
    if (!draft) return;
    const tokens = { ...draft.tokens }; const invalid = { ...invalidValues };
    delete tokens[key]; delete invalid[key]; invalidValues = invalid;
    draft = { ...draft, tokens }; renderPreview();
  }
  function resetAll() { if (draft) { invalidValues = {}; draft = { ...draft, tokens: {} }; renderPreview(); } }
  function coordinateAccent() {
    if (!draft || invalidValues["--accent"] !== undefined) return;
    const next = accentTokens(colorToHex(draft.tokens["--accent"] ?? baseTokens["--accent"] ?? ""));
    const invalid = { ...invalidValues };
    for (const key of Object.keys(next) as CustomTokenKey[]) delete invalid[key];
    invalidValues = invalid; draft = { ...draft, tokens: { ...draft.tokens, ...next } }; renderPreview();
  }
  function cancelEditing() { restoreTheme(); draft = null; invalidValues = {}; error = ""; notice = "Preview discarded. Your saved appearance is unchanged."; }
  function escapePreview(event: KeyboardEvent) {
    if (event.key !== "Escape" || event.defaultPrevented || !draft || !livePreview) return;
    // Consumed: the Settings window closes on an Escape nothing handled.
    event.preventDefault();
    livePreview = false; restoreTheme();
    notice = "Studio preview stopped. Your draft is still available to edit or save.";
  }
  function save(asNew = false) {
    if (!draft || invalidCount) return;
    error = "";
    try {
      const saved = saveScheme({ ...draft, ...(asNew ? { id: undefined } : {}), name: asNew ? uniqueName(draft.name.trim()) : draft.name.trim() });
      refresh(); draft = null; invalidValues = {}; notice = `${saved.name} saved and applied.`;
    }
    catch (cause) { explainError(cause); }
  }
  function remove(scheme: ColorScheme) {
    importGeneration++; importing = false;
    error = "";
    try { deleteScheme(scheme.id); refresh(); pendingDelete = null; notice = `${scheme.name} deleted. Built-in palettes are always available.`; }
    catch (cause) { explainError(cause); }
  }
  function download(scheme: ColorScheme) {
    error = "";
    try {
      downloadJSON(exportScheme(scheme), `${scheme.name.replace(/[^a-z0-9_-]/gi, "-").replace(/-+/g, "-") || "bakery"}.bakery-theme.json`);
      notice = `${scheme.name} exported. Import the file to reuse or share it.`;
    } catch (cause) { explainError(cause); }
  }
  function downloadJSON(json: string, filename: string) {
    const url = URL.createObjectURL(new Blob([json], { type: "application/json" }));
    const link = document.createElement("a"); link.href = url; link.download = filename;
    link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  function backupLibrary() {
    error = "";
    try { downloadJSON(exportAppearanceBackup(), "bakery-color-library-backup.json"); }
    catch (cause) { explainError(cause); }
  }
  function resetLibrary() {
    error = ""; importGeneration++; importing = false;
    try {
      resetAppearanceLibrary(); draft = null; invalidValues = {}; confirmReset = false; refresh();
      notice = "Color library reset. Bakery’s original Dark palette is active.";
    } catch (cause) { explainError(cause); }
  }
  async function upload(event: Event) {
    const input = event.currentTarget as HTMLInputElement; const file = input.files?.[0]; input.value = "";
    if (!file || draft) return;
    const generation = ++importGeneration;
    importing = true;
    error = ""; notice = "";
    try {
      if (file.size > 64_000) throw new Error("Choose a color scheme JSON file smaller than 64 KB.");
      const json = await file.text(); if (!alive || draft || generation !== importGeneration) return;
      const saved = importScheme(json); refresh(); notice = `${saved.name} imported and applied.`;
    } catch (cause) { if (alive && generation === importGeneration) explainError(cause); }
    finally { if (alive && generation === importGeneration) importing = false; }
  }
</script>

<svelte:window onkeydown={escapePreview} />

<!-- Settings rows (settings.css) around the palette pickers: the current
     scheme, the built-in and saved palettes as radio tiles, and the editor
     as rows of fields over hairline-split colour lists. -->
<div class="appearance set-panel">
  <p class="set-lead">Choose a palette or make one entirely your own. Changes apply across Studio, Baker, the editor, and terminals on this device.</p>
  <div class="status">
    <div class="set-row current">
      <span class="set-row-text">
        <span class="set-row-title">Current scheme</span>
        {#if !(draft && livePreview)}<span class="set-row-desc">Saved on this device</span>{/if}
      </span>
      <span class="set-row-control">
        {#if draft && livePreview}<span class="set-chip warn">Unsaved preview</span>{/if}
        <strong class="current-name"><span class="current-dot" aria-hidden="true"></span>{themeLabel(appearance.selected, appearance)}</strong>
      </span>
    </div>
    {#if storageIssue}
      <div class="set-callout bad" role="alert">
        <p>{storageIssue}</p>
        <div class="recovery-actions"><button class="set-btn" type="button" onclick={backupLibrary}>Download saved library backup</button><button class="set-btn destructive" type="button" onclick={() => { confirmReset = true; }}>Reset color library</button></div>
        {#if confirmReset}<div class="recovery-confirmation" role="group" aria-label="Confirm color library reset"><p>This deletes all saved custom schemes on this device and returns to Dark. Download a backup first if you want to keep the stored data.</p><div class="recovery-actions"><button class="set-btn" type="button" onclick={() => { confirmReset = false; }}>Keep library</button><button class="set-btn destructive" type="button" onclick={resetLibrary}>Reset library</button></div></div>{/if}
      </div>
    {/if}
    {#if error}<div class="set-callout bad" role="alert">{error}</div>{/if}
    {#if notice}<div class="set-callout ok" role="status">{notice}</div>{/if}
  </div>
  <section class="set-section" aria-labelledby="builtin-heading">
    <div class="set-section-hd"><div><h2 id="builtin-heading" class="set-section-h">Bakery palettes</h2><p class="set-section-desc">A starting point, never a limit.</p></div><span class="section-meta">{BUILTIN_THEMES.length + 1} built-in schemes</span></div>
    <div class="cards" role="radiogroup" aria-label="Color scheme">
      <label class="scheme-card" class:selected={appearance.selected === "auto"} class:unavailable={draft !== null}>
        <input type="radio" name="appearance-scheme" aria-label={AUTO_LABEL} checked={appearance.selected === "auto"} disabled={draft !== null} onclick={(event) => { if (!choose("auto")) event.preventDefault(); }} />
        <SchemePreview base="warm" />
        <span class="scheme-title"><span>{AUTO_LABEL}</span><span class="selection-mark" aria-hidden="true">{appearance.selected === "auto" ? "✓" : ""}</span></span>
        <span class="scheme-blurb">Warm when the Mac is dark, Parchment when it's light. The default.</span>
      </label>
      {#each BUILTIN_THEMES as base (base)}
        <label class="scheme-card" class:selected={appearance.selected === base} class:unavailable={draft !== null}>
          <input type="radio" name="appearance-scheme" aria-label={THEME_LABELS[base]} checked={appearance.selected === base} disabled={draft !== null} onclick={(event) => { if (!choose(base)) event.preventDefault(); }} />
          <SchemePreview {base} />
          <span class="scheme-title"><span>{THEME_LABELS[base]}</span><span class="selection-mark" aria-hidden="true">{appearance.selected === base ? "✓" : ""}</span></span>
          <span class="scheme-blurb">{BLURBS[base]}</span>
        </label>
      {/each}
    </div>
  </section>
  <section class="set-section" aria-labelledby="bakery-heading">
    <div class="set-section-hd"><div><h2 id="bakery-heading" class="set-section-h">Bakery themes to try</h2><p class="set-section-desc">tweakcn themes made for the Bakery. One click applies one.</p></div><span class="section-meta">{BAKERY_THEMES.length} themes</span></div>
    <div class="cards">
      {#each BAKERY_THEMES as t (t.name)}
        {@const on = appearance.schemes.some((s) => s.id === appearance.selected && s.name === t.name)}
        <button class="scheme-card bakery-theme" class:selected={on} type="button" disabled={draft !== null} onclick={() => tryBakeryTheme(t)}>
          <span class="bakery-swatches" aria-hidden="true">{#each bakerySwatches(t) as c}<span style:background={c}></span>{/each}</span>
          <span class="scheme-title"><span>{t.name}</span><span class="selection-mark" aria-hidden="true">{on ? "✓" : ""}</span></span>
          <span class="scheme-blurb">{t.blurb}</span>
        </button>
      {/each}
    </div>
  </section>
  <section class="set-section" aria-labelledby="saved-heading">
    <div class="set-section-hd">
      <div><h2 id="saved-heading" class="set-section-h">Your schemes</h2><p class="set-section-desc">Keep your favorites. Edit, duplicate, or take them with you.</p></div>
      <div class="section-actions"><button class="set-btn" type="button" aria-expanded={tweakOpen} disabled={draft !== null} onclick={() => { tweakOpen = !tweakOpen; }}>Import tweakcn theme</button><button class="set-btn" type="button" aria-label="Import scheme" disabled={draft !== null || importing} onclick={() => importInput?.click()}>{importing ? "Importing…" : "Import scheme"}</button><button class="set-btn primary" type="button" disabled={draft !== null} onclick={() => startEditing()}><span aria-hidden="true">+</span> Create scheme</button></div>
      <input bind:this={importInput} class="file-input" type="file" accept="application/json,.json" aria-label="Import color scheme file" onchange={upload} />
    </div>
    {#if tweakOpen}
      <form class="tweak-import" onsubmit={importTweak}>
        <div class="set-field">
          <label for="tweak-text">tweakcn theme: paste its CSS (Code → CSS) or a registry link (https://tweakcn.com/r/themes/….json)</label>
          <textarea id="tweak-text" class="set-input mono" rows="7" bind:value={tweakText} disabled={tweakBusy}
            placeholder={":root {\n  --background: oklch(…);\n  …\n}\n.dark { … }\n\nor https://tweakcn.com/r/themes/catppuccin.json"}></textarea>
        </div>
        <div class="set-field">
          <label for="tweak-name">Name (optional)</label>
          <input id="tweak-name" class="set-input" bind:value={tweakName} disabled={tweakBusy} placeholder="the theme's own name" />
        </div>
        <p class="set-section-desc">Colors, fonts, corner radius and shadows come across; light and dark follow your Mac. Fonts must be installed on this Mac. Only plain values are taken.</p>
        <div class="section-actions">
          <button class="set-btn" type="button" disabled={tweakBusy} onclick={() => { tweakOpen = false; }}>Cancel</button>
          <button class="set-btn primary" type="submit" disabled={tweakBusy || !tweakText.trim()}>{tweakBusy ? "Importing…" : "Import"}</button>
        </div>
      </form>
    {/if}
    {#if appearance.schemes.length === 0}
      <div class="empty-schemes"><span class="empty-icon" aria-hidden="true">◈</span><div><strong>A space that feels like you.</strong><p>Create a scheme from your current palette. Fine-tune a single accent or every color.</p></div></div>
    {:else}
      <div class="cards saved-cards" role="radiogroup" aria-label="Saved color schemes">
        {#each appearance.schemes as scheme (scheme.id)}
          <article class="saved-card" class:selected={appearance.selected === scheme.id}>
            <label class="scheme-card" class:unavailable={draft !== null}>
              <input type="radio" name="appearance-scheme" aria-label={scheme.name} checked={appearance.selected === scheme.id} disabled={draft !== null} onclick={(event) => { if (!choose(scheme.id)) event.preventDefault(); }} />
              <SchemePreview base={scheme.base} tokens={scheme.tokens} />
              <span class="scheme-title"><span>{scheme.name}</span><span class="selection-mark" aria-hidden="true">{appearance.selected === scheme.id ? "✓" : ""}</span></span>
              <span class="scheme-blurb">{scheme.dark ? "Light and dark, following your Mac" : `Based on ${THEME_LABELS[scheme.base]}`} · {Object.keys(scheme.tokens).length} custom colors</span>
            </label>
            <div class="scheme-actions">
              <button type="button" aria-label={`Edit ${scheme.name}`} disabled={draft !== null} onclick={() => startEditing(scheme)}>Edit</button>
              <button type="button" aria-label={`Duplicate ${scheme.name}`} disabled={draft !== null} onclick={() => startEditing(scheme, true)}>Duplicate</button>
              <button type="button" aria-label={`Export ${scheme.name}`} onclick={() => download(scheme)}>Export</button>
              <button type="button" aria-label={`Delete ${scheme.name}`} disabled={draft !== null} onclick={() => { pendingDelete = scheme.id; }}>Delete</button>
            </div>
            {#if pendingDelete === scheme.id}
              <div class="delete-confirmation" role="group" aria-label={`Confirm deletion of ${scheme.name}`}>
                <p>Delete “{scheme.name}”?{appearance.selected === scheme.id ? ` Studio will return to ${THEME_LABELS[scheme.base]}.` : " This cannot be undone."}</p>
                <div><button class="set-btn sm" type="button" onclick={() => { pendingDelete = null; }}>Keep scheme</button><button class="set-btn sm destructive" type="button" onclick={() => remove(scheme)}>Delete scheme</button></div>
              </div>
            {/if}
          </article>
        {/each}
      </div>
    {/if}
  </section>
  {#if draft}
    <section class="set-section editor" aria-labelledby="editor-heading">
      <div class="set-section-hd"><h2 id="editor-heading" class="set-section-h">{draft.id ? "Make it your own" : "Create your color scheme"}</h2><span class="section-meta">{changedCount} customized</span></div>
      <div class="editor-intro">
        <div class="editor-fields">
          <div class="set-row">
            <span class="set-row-text"><label class="set-row-title" for="scheme-name">Scheme name</label></span>
            <input bind:this={nameInput} id="scheme-name" class="set-input text-input" type="text" bind:value={draft.name} maxlength="80" placeholder="Give your palette a name" />
          </div>
          <div class="set-row">
            <span class="set-row-text"><label class="set-row-title" for="scheme-base">Base palette</label><span class="set-row-desc">Unchanged colors follow this palette. Your custom colors are kept when switching bases.</span></span>
            <select id="scheme-base" class="set-select" value={draft.base} onchange={(event) => changeBase(event.currentTarget.value as BuiltinTheme)}>{#each BUILTIN_THEMES as base}<option value={base}>{THEME_LABELS[base]}</option>{/each}</select>
          </div>
          <div class="set-row">
            <span class="set-row-text"><label class="set-row-title" for="scheme-live-preview">Preview in Studio</label><span class="set-row-desc">Preview is temporary. Nothing is saved until you choose Save scheme. Press Esc to stop previewing.</span></span>
            <input id="scheme-live-preview" class="set-switch" type="checkbox" checked={livePreview} onchange={(event) => { livePreview = event.currentTarget.checked; renderPreview(); }} />
          </div>
        </div>
        <div class="editor-preview"><SchemePreview base={draft.base} tokens={draft.tokens} large /><span>Live palette preview</span></div>
      </div>
      <div class="set-section-hd color-heading"><div><h3 class="set-subhead">Fine-tune your colors</h3><p class="set-section-desc">Use the picker for solid colors, or enter hex, RGB, or HSL values with optional opacity.</p></div><button class="set-btn" type="button" disabled={changedCount === 0 && invalidCount === 0} onclick={resetAll}>Reset all colors</button></div>
      <div class="token-groups">
        {#each tokenGroups as group (group.label)}
          <details class="token-group" open={group.label === "Surfaces" || group.label === "Accent"}>
            <summary><span><strong>{group.label}</strong><span class="group-description">{group.description}</span></span><span class="group-count">{group.keys.filter((key) => draft?.tokens[key] !== undefined).length}/{group.keys.length}<span class="chevron" aria-hidden="true">⌄</span></span></summary>
            {#if group.label === "Accent"}<div class="accent-helper"><span>Start with “Accent color,” then coordinate the related button, border, and tint colors.</span><button class="set-btn sm" type="button" disabled={invalidValues["--accent"] !== undefined} onclick={coordinateAccent}>Apply to accent family</button></div>{/if}
            <div class="token-rows">
              {#each group.keys as key (key)}
                <div class="token-row" class:overridden={draft.tokens[key] !== undefined}>
                  <div class="token-name"><label for={`color-text-${key}`}>{tokenLabel(key)}</label><span title={key}>{key}</span></div>
                  <div class="token-controls">
                    <input class="color-picker" type="color" aria-label={`${tokenLabel(key)} picker`} value={colorToHex(draft.tokens[key] ?? baseTokens[key] ?? "")} oninput={(event) => updateToken(key, event.currentTarget.value)} />
                    <input id={`color-text-${key}`} class="color-value" type="text" spellcheck="false" aria-invalid={invalidValues[key] !== undefined} aria-describedby={invalidValues[key] !== undefined ? `color-error-${key}` : undefined} value={tokenValue(key)} oninput={(event) => updateToken(key, event.currentTarget.value)} />
                    <button class="reset-token" type="button" aria-label={`Reset ${tokenLabel(key)}`} title="Use the base palette color" disabled={draft.tokens[key] === undefined && invalidValues[key] === undefined} onclick={() => resetToken(key)}>↺</button>
                  </div>
                  {#if invalidValues[key] !== undefined}<span id={`color-error-${key}`} class="color-error">Enter a valid color, for example #5b8cff or rgba(91, 140, 255, 0.2).</span>{/if}
                </div>
              {/each}
            </div>
          </details>
        {/each}
      </div>
      <footer class="editor-footer"><span>{invalidCount ? `Fix ${invalidCount} invalid color${invalidCount === 1 ? "" : "s"} before saving.` : "Your built-in palette stays untouched."}</span><div><button class="set-btn" type="button" onclick={cancelEditing}>Cancel editing</button>{#if draft.id}<button class="set-btn" type="button" disabled={!draft.name.trim() || invalidCount > 0} onclick={() => save(true)}>Save as new scheme</button>{/if}<button class="set-btn primary" type="button" disabled={!draft.name.trim() || invalidCount > 0} onclick={() => save()}>Save scheme</button></div></footer>
    </section>
  {/if}
  <p class="set-muted footnote">Appearance is a device preference, not an agent instruction. Saved schemes survive reloads and pane changes. Export a scheme to keep a portable copy.</p>
</div>
<style>
  /* Rows, text and controls come from the shared settings vocabulary; the
     palette tiles, the editor's colour lists and its sticky footer are this
     page's own. Layout follows the content column (`@container settings`). */
  .status { display: flex; flex-direction: column; gap: 10px; }
  .current-name { display: inline-flex; align-items: center; gap: 8px; color: var(--fg); font: 600 13px/1.4 var(--font-ui); overflow-wrap: anywhere; }
  .current-dot { width: 8px; height: 8px; flex: 0 0 auto; border-radius: 50%; background: var(--accent); box-shadow: 0 0 0 3px var(--accent-bg); }
  .set-callout p { margin: 0; }
  .recovery-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 10px; }
  .recovery-confirmation { margin-top: 12px; padding-top: 12px; border-top: 1px solid var(--bad-border); }
  .section-meta { color: var(--fg-faint); font: 400 11.5px/1.5 var(--font-ui); white-space: nowrap; }
  .section-actions { display: flex; flex-wrap: wrap; gap: 8px; }
  .file-input { display: none; }

  .cards { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; margin-top: 8px; }
  .scheme-card { position: relative; display: flex; flex-direction: column; gap: 9px; min-width: 0; padding: 8px; border: 1px solid var(--line-soft); border-radius: var(--r-xl); background: transparent; cursor: pointer; transition: border-color 120ms, background 120ms; }
  .scheme-card:hover:not(.unavailable) { border-color: var(--line-strong); background: var(--bg-raised); }
  .scheme-card.selected, .saved-card.selected { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent-bg-2); }
  .scheme-card:has(input:focus-visible) { outline: 2px solid var(--accent); outline-offset: 3px; }
  .scheme-card > input { position: absolute; inset: 0; z-index: 1; width: 100%; height: 100%; padding: 0; margin: 0; opacity: 0; cursor: pointer; }
  .scheme-card > input:disabled { cursor: default; }
  .scheme-title { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 0 2px; color: var(--fg); font: 500 12.5px/1.35 var(--font-ui); }
  .scheme-title > span:first-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .selection-mark { display: grid; place-content: center; width: 14px; height: 14px; flex: 0 0 auto; border-radius: 50%; border: 1px solid var(--line-strong); color: var(--accent-fg); font-size: 9px; }
  .selected .selection-mark { background: var(--accent-bg); border-color: var(--accent); }
  .scheme-blurb { display: block; min-height: 32px; padding: 0 2px 2px; color: var(--fg-dim); font: 400 11px/1.45 var(--font-ui); }
  .unavailable { cursor: default; }
  .bakery-theme { text-align: left; font: inherit; color: inherit; }
  .bakery-swatches { display: flex; height: 44px; overflow: hidden; border-radius: var(--r-lg); border: 1px solid var(--line-soft); }
  .bakery-swatches > span { flex: 1; }
  .bakery-swatches > span:first-child { flex: 2; }

  .tweak-import { display: grid; gap: 10px; margin: 4px 0 14px; padding: 12px; border: 1px solid var(--line-soft); border-radius: var(--r-xl); background: var(--bg-raised); }
  .tweak-import .section-actions { justify-content: flex-end; }
  .empty-schemes { display: flex; align-items: center; gap: 14px; padding: 12px 0; }
  .empty-icon { width: 34px; height: 34px; display: grid; place-content: center; flex: 0 0 auto; border-radius: var(--r-lg); color: var(--accent-fg); background: var(--accent-bg); font-size: 20px; }
  .empty-schemes strong { color: var(--fg); font: 450 13px/1.4 var(--font-ui); }
  .empty-schemes p { margin: 3px 0 0; color: var(--fg-dim); font: 400 12px/1.5 var(--font-ui); }

  .saved-card { min-width: 0; align-self: start; border: 1px solid var(--line-soft); border-radius: var(--r-xl); overflow: hidden; }
  .saved-card .scheme-card { border: 0; border-bottom: 1px solid var(--line-soft); border-radius: 0; }
  .saved-card .scheme-blurb { min-height: 0; }
  .scheme-actions { display: flex; flex-wrap: wrap; gap: 1px; padding: 5px; }
  .scheme-actions button { border: 0; background: transparent; padding: 5px 6px; border-radius: var(--r-sm); color: var(--fg-dim); font: 400 11px/1.3 var(--font-ui); cursor: pointer; }
  .scheme-actions button:hover:not(:disabled) { color: var(--fg); background: var(--bg-chip); }
  .scheme-actions button:disabled { opacity: .45; cursor: default; }
  .scheme-actions button:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .delete-confirmation { border-top: 1px solid var(--bad-border); padding: 10px; background: var(--bad-bg); }
  .delete-confirmation p { margin: 0; color: var(--fg-body); font: 400 12px/1.5 var(--font-ui); overflow-wrap: anywhere; }
  .delete-confirmation > div { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 10px; }

  /* ---- editor: field rows beside the live preview, then the colours ---- */
  .editor-intro { display: grid; grid-template-columns: minmax(0, 1fr) 260px; align-items: start; gap: 24px; }
  .editor-fields { min-width: 0; }
  .text-input { width: 220px; }
  .editor-preview { min-width: 0; padding-top: 12px; }
  .editor-preview > span { display: block; margin-top: 8px; color: var(--fg-faint); text-align: center; font: 400 11px/1.4 var(--font-ui); }
  .color-heading { margin-top: 24px; }
  .token-groups { margin-top: 6px; }
  .token-group { border-top: 1px solid var(--line-soft); }
  .token-group:last-child { border-bottom: 1px solid var(--line-soft); }
  summary { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin: 0 -8px; padding: 12px 8px; border-radius: var(--r-lg); cursor: pointer; list-style: none; }
  summary::-webkit-details-marker { display: none; }
  summary:hover { background: var(--bg-raised); }
  summary:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  summary strong { color: var(--fg); font: 450 13px/1.4 var(--font-ui); }
  .group-description { display: block; margin-top: 2px; color: var(--fg-dim); font: 400 12px/1.5 var(--font-ui); }
  .group-count { display: flex; align-items: center; gap: 12px; color: var(--fg-faint); font: 400 11px/1.4 var(--font-mono); }
  .chevron { display: block; font: 500 15px/1 var(--font-ui); transform: rotate(-90deg); transition: transform 120ms; }
  details[open] .chevron { transform: rotate(0); }
  .token-rows { padding: 0 0 8px; }
  .token-row { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 8px 18px; padding: 9px 0; border-top: 1px solid var(--line-soft); }
  .token-name { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .token-name label { color: var(--fg-2); font: 400 12.5px/1.4 var(--font-ui); }
  .overridden .token-name label::after { content: ""; display: inline-block; width: 4px; height: 4px; border-radius: 50%; background: var(--accent); margin: 0 0 2px 7px; }
  .token-name > span { color: var(--fg-faint); font: 400 10px/1.4 var(--font-mono); text-overflow: ellipsis; white-space: nowrap; overflow: hidden; }
  .token-controls { display: flex; align-items: center; gap: 6px; }
  .color-picker { display: block; width: 30px; height: 30px; padding: 2px; border: 1px solid var(--line); border-radius: var(--r-md); background: transparent; cursor: pointer; }
  .color-picker::-webkit-color-swatch-wrapper { padding: 1px; }
  .color-picker::-webkit-color-swatch { border: 0; border-radius: 4px; }
  .color-value { width: 181px; min-width: 0; height: 30px; border: 1px solid var(--line); border-radius: var(--r-md); background: transparent; color: var(--fg-body); padding: 0 8px; font: 400 11px/1.4 var(--font-mono); }
  .color-value[aria-invalid="true"] { border-color: var(--bad); }
  .color-picker:focus-visible, .color-value:focus-visible { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-bg); }
  .reset-token { width: 26px; height: 30px; padding: 0; border: 0; border-radius: var(--r-md); background: transparent; color: var(--fg-dim); font-size: 17px; cursor: pointer; }
  .reset-token:hover:not(:disabled) { background: var(--bg-chip); color: var(--fg); }
  .reset-token:disabled { opacity: .25; cursor: default; }
  .reset-token:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .color-error { grid-column: 1 / -1; color: var(--bad); font: 400 11px/1.5 var(--font-ui); }
  .accent-helper { display: flex; align-items: center; justify-content: space-between; gap: 14px; margin: 0 0 10px; padding: 10px 12px; border-radius: var(--r-lg); background: var(--accent-bg); }
  .accent-helper > span { color: var(--fg-2); font: 400 12px/1.5 var(--font-ui); }
  .editor-footer { position: sticky; bottom: 0; z-index: 1; display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 12px; margin-top: 12px; padding: 12px 0; border-top: 1px solid var(--line); background: var(--bg-app); }
  .editor-footer > span { color: var(--fg-dim); font: 400 12px/1.5 var(--font-ui); }
  .editor-footer > div { display: flex; flex-wrap: wrap; gap: 8px; }
  .footnote { max-width: 620px; }

  @container settings (max-width: 640px) {
    .cards { grid-template-columns: repeat(2, minmax(0, 1fr)); }
    .editor-intro { grid-template-columns: minmax(0, 1fr); gap: 8px; }
    .editor-preview { max-width: 380px; width: 100%; }
    .color-value { width: 145px; }
  }
  @container settings (max-width: 520px) {
    .section-meta { display: none; }
    .cards { gap: 8px; }
    .scheme-card { padding: 6px; }
    .scheme-title { font-size: 11.5px; }
    .scheme-blurb { font-size: 10px; }
    .text-input { width: auto; flex: 1 1 200px; }
    .token-row { grid-template-columns: 1fr; gap: 6px; padding: 12px 0; }
    .token-name { flex-direction: row; justify-content: space-between; align-items: center; gap: 8px; }
    .token-controls { width: 100%; }
    .color-value { flex: 1; width: 0; }
    .color-picker { flex: 0 0 32px; width: 32px; height: 32px; }
    .color-value, .reset-token { height: 32px; }
    .accent-helper { flex-wrap: wrap; gap: 9px; }
    .editor-footer > div { margin-left: auto; }
    .empty-schemes { gap: 12px; }
  }
  @media (prefers-reduced-motion: reduce) { .scheme-card, .chevron { transition: none; } }
</style>

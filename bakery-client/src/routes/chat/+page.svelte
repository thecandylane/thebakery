<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { goto } from "$app/navigation";
  import { node, type ModelCatalog } from "$lib/node";
  import { studioStore } from "$lib/studioStore.svelte";
  import { chatPath, chats } from "$lib/conversations";
  import { mountChatStart } from "$lib/react/mount";
  import {
    createChatStartSource,
    NEW_CHAT,
    type ChatStartFile,
    type ChatStartSnapshot,
  } from "$lib/react/chatStartSource.svelte";
  import WorkBoard from "$lib/shell/WorkBoard.svelte";

  // Chat home: say what you want and it starts — by default as a new chat
  // (its own workspace inside Bakery, on the model you pick), or as the next
  // message in any chat you choose. The Baker is not here: it has its own
  // mode. The composer is the agent pane's AI Elements prompt (a React
  // island, `ChatStart.tsx`) drawing what this page decides; below, every
  // chat by where it stands (the AI Elements work board), with the same
  // actions as everywhere.

  const MODEL_KEY = "bakery.chat.model";

  const list = $derived(chats(studioStore.data));
  /** Models this node can run right now. */
  const models = $derived((studioStore.data?.model_options ?? []).filter((m) => m.node_available));
  let model = $state(rememberedModel());
  /** The remembered pick, or the first runnable model once it is gone. */
  const newChatModel = $derived(models.some((m) => m.id === model) ? model : (models[0]?.id ?? ""));

  function rememberedModel(): string {
    try {
      return localStorage.getItem(MODEL_KEY) ?? "";
    } catch {
      return "";
    }
  }

  function pickModel(id: string) {
    model = id;
    variant = "";
    effort = "";
    remember(MODEL_KEY, id);
  }

  function remember(key: string, value: string) {
    try {
      localStorage.setItem(key, value);
    } catch {
      // a per-viewer convenience only
    }
  }

  // The account's own models and effort (Opus / Sonnet …, Low … Max): read
  // from the account — for a native account, from any live session on it.
  const VARIANT_KEY = "bakery.chat.variant";
  const EFFORT_KEY = "bakery.chat.effort";
  let catalog = $state<ModelCatalog | null>(null);
  /** The account's model list is being read — a native account may need its
   *  adapter started to say, so this can take a few seconds. */
  let catalogLoading = $state(false);
  /** Why the list could not be read at all (the node did not answer). */
  let catalogFailure = $state("");
  /** Retry bumps `refresh`; the next read asks the adapter, not its memory. */
  let refresh = $state(0);
  let askAdapter = false;
  /** The account has a list to show (false: no catalog concept — say nothing). */
  let catalogExpected = $state(true);
  let variant = $state(rememberedValue(VARIANT_KEY));
  let effort = $state(rememberedValue(EFFORT_KEY));
  const variants = $derived(catalog?.status === "available" ? catalog.models : []);
  const chosenVariant = $derived(variants.find((v) => v.id === variant) ?? null);
  const efforts = $derived(chosenVariant?.effort_levels ?? []);

  function rememberedValue(key: string): string {
    try {
      return localStorage.getItem(key) ?? "";
    } catch {
      return "";
    }
  }

  $effect(() => {
    const account = newChatModel;
    void refresh;
    catalog = null;
    catalogFailure = "";
    catalogExpected = true;
    catalogLoading = false;
    if (!account) return;
    const force = askAdapter;
    askAdapter = false;
    let current = true;
    catalogLoading = true;
    void node
      .accountCapabilities(account, undefined, undefined, force)
      .then((caps) => {
        if (!current) return;
        catalog = caps.catalog;
        catalogExpected = caps.catalog_expected !== false;
      })
      .catch((e) => {
        if (current) catalogFailure = `${e}`.replace(/^Error:\s*/, "");
      })
      .finally(() => {
        if (current) catalogLoading = false;
      });
    return () => {
      current = false;
    };
  });
  const modelsState = $derived<"loading" | "ready" | "unavailable" | "none">(
    !newChatModel || !catalogExpected
      ? "none"
      : catalogLoading
        ? "loading"
        : catalog?.status === "available"
          ? "ready"
          : "unavailable",
  );
  const modelsReason = $derived(
    catalogFailure || (catalog?.status === "unavailable" ? catalog.reason : ""),
  );
  let target = $state(NEW_CHAT);
  let sending = $state(false);
  let error = $state("");
  let host: HTMLDivElement | undefined = $state();

  const chosen = $derived(target === NEW_CHAT ? null : (list.active.find((c) => c.streamId === target) ?? null));

  /** The model an existing chat's message goes to. */
  const chosenModelId = $derived.by(() => {
    const conv = chosen;
    if (!conv) return "";
    return studioStore.findSocket(conv.streamId)?.agents.find((a) => a.role_id === conv.roleId)?.assigned_model_id ?? "";
  });
  const modelLabel = $derived.by(() => {
    if (!chosen) return "";
    if (!chosenModelId) return "no model assigned";
    return studioStore.data?.model_options.find((m) => m.id === chosenModelId)?.label ?? chosenModelId;
  });
  /** Sends one message where the composer points. Resolving is what clears
   *  the composer's draft, so anything short of "the node has it" rejects. */
  async function send(text: string, files: readonly ChatStartFile[] = []): Promise<void> {
    const instruction = text.trim();
    if ((!instruction && files.length === 0) || sending) throw new Error("Nothing to send.");
    sending = true;
    error = "";
    let sent = false;
    let created = "";
    // Files move into the chat's own attachment store once the chat exists.
    const upload = async (streamId: string, seat: string) => {
      const ids: string[] = [];
      for (const file of files) ids.push(...(await node.uploadAttachment(streamId, seat, file)).map((ref) => ref.id));
      return ids;
    };
    try {
      let streamId: string;
      if (chosen) {
        const ids = await upload(chosen.streamId, chosen.roleId);
        await node.queueTurn(chosen.streamId, instruction, "", chosen.roleId, ids);
        streamId = chosen.streamId;
      } else if (target === NEW_CHAT) {
        if (!newChatModel) throw new Error("No model is ready — add one in Settings → Provider accounts.");
        const firstLine = instruction.split("\n").find((l) => l.trim())?.trim();
        const title = (firstLine || files[0]?.filename || "New chat").slice(0, 60);
        created = (await node.chatCreate(title)).stream_id;
        streamId = created;
        studioStore.refresh();
        await node.seatAssign(streamId, "orchestrator", newChatModel);
        // The model and effort picked here are the new chat's selection: its
        // every turn runs on them, and its thread shows them.
        const chosenEffort = efforts.find((e) => e.id === effort);
        if (chosenVariant || chosenEffort) {
          await node.select(streamId, "orchestrator", {
            model: chosenVariant?.id ?? "",
            modelLabel: chosenVariant?.label,
            effort: chosenEffort?.id ?? "",
            effortLabel: chosenEffort?.label,
          });
        }
        const ids = await upload(streamId, "orchestrator");
        await node.queueTurn(streamId, instruction, "", "orchestrator", ids);
      } else {
        throw new Error("That chat is no longer active — pick another.");
      }
      sent = true;
      studioStore.refresh();
      await goto(chatPath({ streamId }));
    } catch (e) {
      error = `${e}`.replace(/^Error:\s*/, "");
      // Once the node has the message, keeping the draft would only invite
      // sending it twice: just a failed send keeps it.
      if (!sent) throw e;
    } finally {
      // The chat exists from here on: a retry goes to it, never to a second
      // one. Pointed at only now, so the composer does not flip mid-send.
      if (created) target = created;
      sending = false;
    }
  }

  /** The page's state as plain values: no Svelte proxy crosses into React. */
  function read(): ChatStartSnapshot {
    return {
      target,
      chats: list.active.map((c) => ({ id: c.streamId, label: c.place ? `${c.title} · ${c.place}` : c.title })),
      accounts: models.map((m) => ({ id: m.id, label: m.label })),
      account: newChatModel,
      models: variants.map((v) => ({ id: v.id, label: v.label })),
      model: chosenVariant?.id ?? "",
      efforts: efforts.map((l) => ({ id: l.id, label: l.label })),
      effort: efforts.some((l) => l.id === effort) ? effort : "",
      chatModel: modelLabel,
      modelsState,
      modelsReason,
      sending,
      error,
    };
  }

  // Mounts the composer once; `read` keeps it current from then on.
  $effect(() => {
    const el = host;
    if (!el) return;
    return untrack(() =>
      mountChatStart(
        el,
        createChatStartSource(read, {
          setTarget: (id) => {
            target = id;
          },
          setAccount: pickModel,
          setModel: (id) => {
            variant = id;
            remember(VARIANT_KEY, id);
          },
          setEffort: (id) => {
            effort = id;
            remember(EFFORT_KEY, id);
          },
          retryModels: () => {
            askAdapter = true;
            refresh++;
          },
          send,
        }),
      ),
    );
  });

  onMount(() => {
    studioStore.ensureStarted();
  });
</script>

<div class="home" data-testid="chat-home">
  <div class="hero">
    <h1><span class="mark" aria-hidden="true">B</span>How can I help you today?</h1>

    <div bind:this={host}></div>

    <div class="chips">
      <a class="chip" href="/?open=project" title="Open a folder or repository as a project">
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2 4.5h4l1.5 1.5H14v6.5H2z" /></svg>
        Project or folder
      </a>
      <a class="chip" href="/settings/machine-access" title="What agents may reach on this machine">
        <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="2.5" y="3" width="11" height="8" rx="1.5" /><path d="M6 13.5h4" /></svg>
        Machine access
      </a>
    </div>
  </div>

  <section class="active" aria-label="Your chats">
    <WorkBoard scope={{ kind: "chats" }} emptyText="No chats yet." />
    {#if list.archived.length > 0}
      <p class="archived-note"><a href="/?view=archived">{list.archived.length} archived</a></p>
    {/if}
  </section>
</div>

<style>
  .home {
    height: 100%;
    overflow-y: auto;
    padding: clamp(16px, 5vh, 48px) 20px 32px;
    box-sizing: border-box;
  }
  .hero,
  .active {
    width: 100%;
    max-width: 720px;
    margin: 0 auto;
  }
  h1 {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 12px;
    margin: 0 0 12px;
    font: 500 clamp(24px, 3.4vw, 32px)/1.2 var(--font-display, var(--font-ui));
    letter-spacing: -0.02em;
    color: var(--fg);
  }
  .mark {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 9px;
    background: var(--accent-solid);
    color: var(--fg-on-solid);
    font: 750 15px/1 var(--font-ui);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
    margin-top: 10px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    border: 1px solid var(--line);
    border-radius: var(--r-pill);
    color: var(--fg-2);
    font: 500 12px/1 var(--font-ui);
    text-decoration: none;
  }
  .chip:hover { background: var(--bg-chip); color: var(--fg); text-decoration: none; }
  .chip svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.3; stroke-linejoin: round; }

  .active { margin-top: clamp(20px, 4vh, 36px); }
  .archived-note {
    margin: 12px 8px 0;
    font: 400 12px/1.5 var(--font-ui);
    color: var(--fg-faint);
  }
</style>

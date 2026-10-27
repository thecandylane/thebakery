<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import {
    authHeaders,
    HTTP,
    node,
    SYSTEM_ACCOUNTS_STREAM_ID,
    type ModelOption,
    type AcpPlugConfig,
    type OpenAiCompatPlugConfig,
    type CliAccountStatus,
    type ClaudeNativeStatus,
    type LoginCommand,
    type ModelProxyStatus,
    type ModelKeys,
    type ModelKeyProvider,
  } from "$lib/node";
  import { keep } from "$lib/kept";
  import Terminal from "$lib/widgets/Terminal.svelte";

  // PANELS.md "Provider accounts" — the former /setup page, restyled to
  // tokens. Endpoints, device-OAuth polling and the Gemini CLI import are
  // ported verbatim; only the markup changed.
  //
  // SECURITY: the node's ProviderStatus DOES carry an optional `oauth_email`
  // (crates/bakery-agent/src/secrets.rs) and SECURITY.md permits it on
  // /auth/status. This screen deliberately does not declare it in AuthStatus
  // below and never renders it: linked / API key set / not linked is the
  // whole vocabulary. Do not add it back — "signed in as <person>" is
  // identity, and this panel shows capability. Secret values are never sent
  // by the node at all and are never echoed back after submit.

  type ProviderStatus = {
    api_key_set: boolean;
    oauth_set: boolean;
    model_id?: string;
    // oauth_email intentionally omitted — see note above.
  };

  type AuthStatus = {
    active_model: string;
    resolved_hint: string;
    xai: ProviderStatus;
    gemini: ProviderStatus;
    anthropic: ProviderStatus;
    openrouter: ProviderStatus;
  };

  type ProviderKey = "xai" | "gemini" | "anthropic" | "openrouter";

  const PROVIDERS: {
    key: ProviderKey;
    name: string;
    initial: string;
    tint: string;
    keyLabel: string;
  }[] = [
    { key: "xai", name: "Grok / xAI", initial: "X", tint: "var(--accent)", keyLabel: "xAI API key" },
    { key: "gemini", name: "Gemini", initial: "G", tint: "var(--ok)", keyLabel: "Gemini API key" },
    {
      key: "anthropic",
      name: "Claude Code / Anthropic",
      initial: "C",
      tint: "var(--violet)",
      keyLabel: "Anthropic API key (Console)",
    },
    { key: "openrouter", name: "OpenRouter", initial: "O", tint: "var(--warn)", keyLabel: "OpenRouter API key" },
  ];

  // 07d-A3D1: Codex/Cursor as first-class accounts. Deliberately not folded
  // into `PROVIDERS`/`AuthStatus` above — neither has an API-key concept,
  // and their status comes from a separate, independently-cached route
  // (`/auth/cli-status`) so a hung binary never slows the other rows.
  type CliTool = "codex" | "cursor";
  const CLI_TOOLS: { key: CliTool; name: string; initial: string; tint: string; command: LoginCommand }[] = [
    { key: "codex", name: "Codex", initial: "X", tint: "var(--fg-dim)", command: "codex-login" },
    { key: "cursor", name: "Cursor", initial: "▲", tint: "var(--fg-dim)", command: "cursor-login" },
  ];
  let cli = $state<Record<CliTool, CliAccountStatus>>({
    codex: { installed: false, linked: false },
    cursor: { installed: false, linked: false },
  });
  let cliLoginTool = $state<CliTool | null>(null);
  let cliLoginTerminalId = $state<string | null>(null);
  let cliNotice = $state("");
  let claudeNative = $state<ClaudeNativeStatus>({
    installed: false,
    connected: false,
    state: "unavailable",
    // Until the node answers: the pin lives in package.json alone (V1).
    adapter_version: "",
    adapter_integrity: "",
    metadata_provenance: "pinned_package_and_native_cli_probe",
  });
  let claudeNativeTerminalId = $state<string | null>(null);

  async function refreshClaudeNative() {
    try {
      claudeNative = await node.claudeNativeStatus();
    } catch {
      /* node down */
    }
  }

  async function signInClaudeNative() {
    cliNotice = "";
    try {
      const info = await node.terminalCreate(
        SYSTEM_ACCOUNTS_STREAM_ID,
        "host",
        undefined,
        "claude-native-login",
      );
      claudeNativeTerminalId = info.terminal_id;
    } catch (err) {
      cliNotice = `request failed: ${err}`;
    }
  }

  async function onClaudeNativeLoginExited(code: number | null) {
    claudeNativeTerminalId = null;
    await refreshClaudeNative();
    if (!claudeNative.connected) {
      cliNotice =
        claudeNative.reason ??
        (code != null
          ? `Claude subscription sign-in did not complete (exited with code ${code}).`
          : "Claude subscription sign-in did not complete.");
    }
  }

  function claudeNativeChip(): { text: string; cls: string } {
    if (claudeNative.connected) return { text: "Connected", cls: "ok" };
    if (!claudeNative.installed) return { text: "Setup needed", cls: "" };
    if (claudeNative.state === "api_key") return { text: "API billing", cls: "" };
    if (claudeNative.state === "external") return { text: "External billing", cls: "" };
    return { text: "Not connected", cls: "" };
  }

  async function refreshCliAccounts(force = false) {
    try {
      cli = await node.cliAccountsStatus(force);
    } catch {
      /* node down */
    }
  }

  /** The node itself decides reuse-vs-spawn, keyed by `LoginCommand` — never
   *  by name (07d-A3D1 review finding 1: a terminal's `name` is free-form
   *  and renameable, so the client must not use it to decide which login
   *  command a session is really running, and must not run its own
   *  check-then-create race against a second concurrent "Sign in" click).
   *  This call is simply idempotent: a live login for `command` already
   *  running is returned as-is, never duplicated. */
  async function signInCli(tool: CliTool, command: LoginCommand) {
    cliNotice = "";
    try {
      const info = await node.terminalCreate(SYSTEM_ACCOUNTS_STREAM_ID, "host", undefined, command);
      cliLoginTool = tool;
      cliLoginTerminalId = info.terminal_id;
    } catch (err) {
      cliNotice = `request failed: ${err}`;
    }
  }

  /** The login terminal closed (07d-A3D1's WS close-reason fix) — drop the
   *  embedded pane and force a fresh status read immediately, rather than
   *  waiting out `/auth/cli-status`'s 10s cache. `code` is the process's
   *  real exit code (or `null` for a signal-only kill) — a closed pane that
   *  still isn't linked is a failed or cancelled sign-in, not silent success,
   *  and must say so (review finding 4). */
  async function onCliLoginExited(tool: CliTool, code: number | null) {
    cliLoginTool = null;
    cliLoginTerminalId = null;
    await refreshCliAccounts(true);
    if (!cli[tool].linked) {
      const name = CLI_TOOLS.find((t) => t.key === tool)?.name ?? tool;
      cliNotice =
        code != null
          ? `${name} sign-in did not complete (exited with code ${code}).`
          : `${name} sign-in did not complete.`;
    }
  }

  function cliChipFor(s: CliAccountStatus): { text: string; cls: string } {
    if (s.linked) return { text: "Linked", cls: "ok" };
    if (s.installed) return { text: "Not linked", cls: "" };
    return { text: "Not installed", cls: "" };
  }

  let auth = $state<AuthStatus | null>(null);
  /** The node's answer, shown here and kept for the composers' provider choice. */
  function setAuth(next: AuthStatus) {
    auth = next;
    keep("auth-status", next);
  }
  let keyProvider = $state<ProviderKey>("xai");
  let keyInput = $state("");
  let modelPick = $state("auto");
  let xaiUserCode = $state("");
  let xaiVerifyUrl = $state("");
  let xaiPollNote = $state("");
  let claudeCodeUrl = $state("");
  let claudeCodeCallback = $state("");
  let claudeCodeNote = $state("");
  let setupNotice = $state("");

  // The model proxy: one door for API-key calls. The operator installs it.
  let proxy = $state<ModelProxyStatus | null>(null);
  let proxyError = $state("");
  let installing = $state(false);
  async function refreshProxy() {
    proxy = await node.modelProxy().catch(() => null);
  }
  async function installProxy() {
    installing = true;
    proxyError = "";
    try {
      proxy = await node.installModelProxy();
    } catch (e) {
      proxyError = `${e}`;
    } finally {
      installing = false;
    }
  }
  async function startProxy() {
    proxyError = "";
    try {
      proxy = await node.startModelProxy();
    } catch (e) {
      proxyError = `${e}`;
    }
  }

  // Its API keys: the operator gives them; Bakery shows labels and dates only.
  const KEY_PROVIDERS: { id: ModelKeyProvider; name: string }[] = [
    { id: "claude", name: "Claude (Anthropic API)" },
    { id: "gemini", name: "Gemini (Google AI Studio)" },
    { id: "xai", name: "Grok (xAI API)" },
  ];
  let modelKeys = $state<ModelKeys | null>(null);
  let newKeyProvider = $state<ModelKeyProvider>("claude");
  let newKeyLabel = $state("");
  let newKeyText = $state("");
  let keyBusy = $state(false);
  async function refreshKeys() {
    modelKeys = await node.modelKeys().catch(() => null);
  }
  async function addKey() {
    keyBusy = true;
    proxyError = "";
    try {
      modelKeys = await node.addModelKey(newKeyProvider, newKeyText, newKeyLabel);
      newKeyText = "";
      newKeyLabel = "";
    } catch (e) {
      proxyError = `${e}`;
    } finally {
      keyBusy = false;
    }
  }
  async function removeKey(provider: ModelKeyProvider, id: string, label: string) {
    if (!confirm(`Remove "${label}" from the model proxy? Its calls use the proxy's other keys, or the provider directly.`))
      return;
    keyBusy = true;
    proxyError = "";
    try {
      modelKeys = await node.removeModelKey(provider, id);
    } catch (e) {
      proxyError = `${e}`;
      void refreshKeys();
    } finally {
      keyBusy = false;
    }
  }
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  // 07d-A3B: named ACP CLI plugs and openai-compat local-model plugs.
  // Definitions (id/argv/base_url/model/...) come from `/providers` — the
  // supported add/edit/remove surface (`node.acpPlugCreate`/`Update`/
  // `Delete` and the openai-compat equivalents); readiness (ready/reason)
  // comes from the same `/studio/state` catalog the Studio picker reads, so
  // this panel can never show a plug — or a readiness verdict — the picker
  // doesn't also see. http rows (grok/claude/gemini) stay in the
  // account-linking list above; this section is the plug rows only.
  let acpPlugs = $state<AcpPlugConfig[]>([]);
  let ocPlugs = $state<OpenAiCompatPlugConfig[]>([]);
  let readiness = $state<Map<string, ModelOption>>(new Map());
  let plugSecretInput = $state<Record<string, string>>({});
  let plugNotice = $state("");

  async function refreshProviders() {
    try {
      const [defs, studio] = await Promise.all([node.providersList(), node.studioState()]);
      acpPlugs = defs.acp_plugs;
      ocPlugs = defs.openai_compat_plugs;
      readiness = new Map(studio.model_options.map((m) => [m.id, m]));
    } catch {
      /* node down */
    }
  }

  async function savePlugSecret(plugId: string) {
    plugNotice = "";
    const key = (plugSecretInput[plugId] ?? "").trim();
    if (!key) return;
    try {
      await node.plugSecretSet(plugId, key);
      plugSecretInput[plugId] = "";
      plugNotice = `saved bearer for ${plugId} (stored on node only)`;
      await refreshProviders();
    } catch (err) {
      plugNotice = `request failed: ${err}`;
    }
  }

  async function clearPlugSecret(plugId: string) {
    plugNotice = "";
    try {
      await node.plugSecretClear(plugId);
      plugNotice = `cleared bearer for ${plugId}`;
      await refreshProviders();
    } catch (err) {
      plugNotice = `request failed: ${err}`;
    }
  }

  // ── ACP plug form (add + edit share one form; id is immutable once
  //    created, so editing an existing row disables that field) ──────────
  let showAcpForm = $state(false);
  let editingAcpId = $state<string | null>(null);
  let acpFormId = $state("");
  let acpFormLabel = $state("");
  let acpFormArgv = $state("");
  let acpFormConfigSource = $state("");
  let acpFormConfigDest = $state("");
  let acpFormEnv = $state("");

  function envToLines(env?: Record<string, string> | null): string {
    if (!env) return "";
    return Object.entries(env)
      .map(([k, v]) => `${k}=${v}`)
      .join("\n");
  }

  function parseEnvLines(s: string): Record<string, string> | undefined {
    const out: Record<string, string> = {};
    for (const line of s.split("\n")) {
      const t = line.trim();
      if (!t) continue;
      const eq = t.indexOf("=");
      if (eq <= 0) continue;
      out[t.slice(0, eq)] = t.slice(eq + 1);
    }
    return Object.keys(out).length ? out : undefined;
  }

  function fillAcpExample(kind: "cursor" | "agy" | "grok") {
    if (kind === "cursor") {
      acpFormId = acpFormId || "acp:cursor";
      acpFormLabel = acpFormLabel || "Cursor";
      acpFormArgv = "cursor-agent acp";
    } else if (kind === "agy") {
      acpFormId = acpFormId || "acp:agy";
      acpFormLabel = acpFormLabel || "Antigravity";
      acpFormArgv = "agy-acp";
      acpFormEnv = "AGY_BIN=";
    } else {
      acpFormId = acpFormId || "acp:grok";
      acpFormLabel = acpFormLabel || "Grok";
      acpFormArgv = "grok agent stdio";
    }
  }

  function parseArgv(s: string): string[] {
    return s.trim().split(/\s+/).filter(Boolean);
  }

  function openAddAcpForm() {
    editingAcpId = null;
    acpFormId = "";
    acpFormLabel = "";
    acpFormArgv = "";
    acpFormConfigSource = "";
    acpFormConfigDest = "";
    acpFormEnv = "";
    showAcpForm = true;
  }

  function openEditAcpForm(p: AcpPlugConfig) {
    editingAcpId = p.id;
    acpFormId = p.id;
    acpFormLabel = p.label ?? "";
    acpFormArgv = p.argv.join(" ");
    acpFormConfigSource = p.config?.source ?? "";
    acpFormConfigDest = p.config?.home_relative_destination ?? "";
    acpFormEnv = envToLines(p.env);
    showAcpForm = true;
  }

  async function submitAcpForm(e: Event) {
    e.preventDefault();
    plugNotice = "";
    const argv = parseArgv(acpFormArgv);
    if (argv.length === 0) {
      plugNotice = "argv must not be empty — the first word is the binary";
      return;
    }
    const config =
      acpFormConfigSource.trim() && acpFormConfigDest.trim()
        ? { source: acpFormConfigSource.trim(), home_relative_destination: acpFormConfigDest.trim() }
        : undefined;
    const label = acpFormLabel.trim() || undefined;
    const env = parseEnvLines(acpFormEnv);
    try {
      if (editingAcpId) {
        await node.acpPlugUpdate(editingAcpId, argv, label, config, env);
        plugNotice = `updated ${editingAcpId}`;
      } else {
        if (!acpFormId.trim()) {
          plugNotice = "id must not be empty";
          return;
        }
        await node.acpPlugCreate(acpFormId.trim(), argv, label, config, env);
        plugNotice = `added ${acpFormId.trim()}`;
      }
      showAcpForm = false;
      editingAcpId = null;
      await refreshProviders();
    } catch (err) {
      plugNotice = `request failed: ${err}`;
    }
  }

  async function deleteAcpPlug(id: string) {
    plugNotice = "";
    try {
      await node.acpPlugDelete(id);
      plugNotice = `removed ${id}`;
      await refreshProviders();
    } catch (err) {
      plugNotice = `request failed: ${err}`;
    }
  }

  // ── openai-compat plug form ─────────────────────────────────────────────
  let showOcForm = $state(false);
  let editingOcId = $state<string | null>(null);
  let ocFormId = $state("");
  let ocFormLabel = $state("");
  let ocFormBaseUrl = $state("");
  let ocFormModel = $state("");

  function openAddOcForm() {
    editingOcId = null;
    ocFormId = "";
    ocFormLabel = "";
    ocFormBaseUrl = "";
    ocFormModel = "";
    showOcForm = true;
  }

  function openEditOcForm(p: OpenAiCompatPlugConfig) {
    editingOcId = p.id;
    ocFormId = p.id;
    ocFormLabel = p.label ?? "";
    ocFormBaseUrl = p.base_url;
    ocFormModel = p.model;
    showOcForm = true;
  }

  async function submitOcForm(e: Event) {
    e.preventDefault();
    plugNotice = "";
    if (!ocFormBaseUrl.trim() || !ocFormModel.trim()) {
      plugNotice = "base_url and model must not be empty";
      return;
    }
    const label = ocFormLabel.trim() || undefined;
    try {
      if (editingOcId) {
        await node.openAiCompatPlugUpdate(editingOcId, ocFormBaseUrl.trim(), ocFormModel.trim(), label);
        plugNotice = `updated ${editingOcId}`;
      } else {
        if (!ocFormId.trim()) {
          plugNotice = "id must not be empty";
          return;
        }
        await node.openAiCompatPlugCreate(ocFormId.trim(), ocFormBaseUrl.trim(), ocFormModel.trim(), label);
        plugNotice = `added ${ocFormId.trim()}`;
      }
      showOcForm = false;
      editingOcId = null;
      await refreshProviders();
    } catch (err) {
      plugNotice = `request failed: ${err}`;
    }
  }

  async function deleteOcPlug(id: string) {
    plugNotice = "";
    try {
      await node.openAiCompatPlugDelete(id);
      plugNotice = `removed ${id}`;
      await refreshProviders();
    } catch (err) {
      plugNotice = `request failed: ${err}`;
    }
  }

  function statusOf(p: ProviderKey): ProviderStatus | null {
    return auth ? auth[p] : null;
  }

  /** linked / API key set / not linked — the entire vocabulary. */
  function chipFor(s: ProviderStatus | null): { text: string; cls: string } {
    if (s?.oauth_set) return { text: "Linked", cls: "ok" };
    if (s?.api_key_set) return { text: "Key set", cls: "ok" };
    return { text: "Not set", cls: "" };
  }

  /** Capability, never identity. No email, no key prefix, no token. */
  function detailFor(s: ProviderStatus | null): string {
    if (!s) return "reading node…";
    const parts: string[] = [];
    if (s.oauth_set) parts.push("account login");
    if (s.api_key_set) parts.push("api key");
    if (parts.length === 0) return "no credential on this node";
    if (s.model_id) parts.push(s.model_id);
    return parts.join(" · ");
  }

  async function refreshNode() {
    try {
      await node.info();
    } catch {
      /* node down */
    }
  }

  async function refreshAuth() {
    try {
      const res = await fetch(`${HTTP}/auth/status`, { headers: authHeaders() });
      if (res.ok) {
        setAuth((await res.json()) as AuthStatus);
        modelPick = auth?.active_model || "auto";
      }
    } catch {
      /* node down */
    }
  }

  async function saveApiKey(e: Event) {
    e.preventDefault();
    setupNotice = "";
    try {
      const res = await fetch(`${HTTP}/auth/api-key`, {
        method: "POST",
        headers: { ...authHeaders(), "content-type": "application/json" },
        body: JSON.stringify({ provider: keyProvider, key: keyInput }),
      });
      const text = await res.text();
      if (!res.ok) {
        setupNotice = `error: ${text}`;
        return;
      }
      setAuth(JSON.parse(text) as AuthStatus);
      // Clear immediately — the key never sits in a live field after submit.
      keyInput = "";
      setupNotice = `saved ${keyProvider} API key (stored on node only)`;
      refreshNode();
    } catch (err) {
      setupNotice = `request failed: ${err}`;
    }
  }

  async function setModel(e: Event) {
    e.preventDefault();
    setupNotice = "";
    try {
      const res = await fetch(`${HTTP}/auth/model`, {
        method: "POST",
        headers: { ...authHeaders(), "content-type": "application/json" },
        body: JSON.stringify({ model: modelPick }),
      });
      const text = await res.text();
      if (!res.ok) {
        setupNotice = `error: ${text}`;
        return;
      }
      setAuth(JSON.parse(text) as AuthStatus);
      setupNotice = `active model → ${auth?.resolved_hint}`;
      refreshNode();
    } catch (err) {
      setupNotice = `request failed: ${err}`;
    }
  }

  async function clearProvider(provider: string) {
    setupNotice = "";
    try {
      const res = await fetch(`${HTTP}/auth/clear`, {
        method: "POST",
        headers: { ...authHeaders(), "content-type": "application/json" },
        body: JSON.stringify({ provider }),
      });
      const text = await res.text();
      if (!res.ok) {
        setupNotice = `error: ${text}`;
        return;
      }
      setAuth(JSON.parse(text) as AuthStatus);
      setupNotice = `cleared ${provider}`;
      refreshNode();
    } catch (err) {
      setupNotice = `request failed: ${err}`;
    }
  }

  async function startXaiLogin() {
    setupNotice = "";
    xaiPollNote = "";
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
    try {
      const res = await fetch(`${HTTP}/auth/xai/device/start`, { method: "POST", headers: authHeaders() });
      const text = await res.text();
      if (!res.ok) {
        setupNotice = `error: ${text}`;
        return;
      }
      const d = JSON.parse(text) as {
        user_code: string;
        verification_uri_complete: string;
        device_code: string;
        interval_seconds: number;
      };
      xaiUserCode = d.user_code;
      xaiVerifyUrl = d.verification_uri_complete;
      xaiPollNote = "waiting for approval in browser…";
      window.open(d.verification_uri_complete, "_blank");
      const intervalMs = Math.max(3, d.interval_seconds || 5) * 1000;
      pollTimer = setInterval(() => pollXai(d.device_code), intervalMs);
    } catch (err) {
      setupNotice = `request failed: ${err}`;
    }
  }

  async function pollXai(deviceCode: string) {
    try {
      const res = await fetch(`${HTTP}/auth/xai/device/poll`, {
        method: "POST",
        headers: { ...authHeaders(), "content-type": "application/json" },
        body: JSON.stringify({ device_code: deviceCode }),
      });
      const text = await res.text();
      if (!res.ok) {
        xaiPollNote = `error: ${text}`;
        return;
      }
      const d = JSON.parse(text) as { status: string; error?: string };
      if (d.status === "pending" || d.status === "slow_down") {
        xaiPollNote =
          d.status === "slow_down" ? "slow down — still waiting…" : "waiting for approval…";
        return;
      }
      if (pollTimer) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
      if (d.status === "complete") {
        xaiPollNote = "Grok account linked";
        xaiUserCode = "";
        await refreshAuth();
        await refreshNode();
        setupNotice = "Grok OAuth complete — model set to grok";
      } else {
        xaiPollNote = d.error || "login failed";
      }
    } catch (err) {
      xaiPollNote = `poll failed: ${err}`;
    }
  }

  async function startClaudeCodeLogin() {
    setupNotice = "";
    claudeCodeNote = "";
    claudeCodeCallback = "";
    try {
      const res = await fetch(`${HTTP}/auth/claude-code/login/start`, { method: "POST", headers: authHeaders() });
      const text = await res.text();
      if (!res.ok) {
        setupNotice = `error: ${text}`;
        return;
      }
      const d = JSON.parse(text) as { verification_uri: string; built: boolean };
      claudeCodeUrl = d.verification_uri;
      claudeCodeNote = "finish sign-in in the browser, then paste the callback here";
      window.open(d.verification_uri, "_blank");
    } catch (err) {
      setupNotice = `request failed: ${err}`;
    }
  }

  async function completeClaudeCodeLogin() {
    setupNotice = "";
    const callback = claudeCodeCallback.trim();
    if (!callback) return;
    try {
      const res = await fetch(`${HTTP}/auth/claude-code/login/complete`, {
        method: "POST",
        headers: { ...authHeaders(), "content-type": "application/json" },
        body: JSON.stringify({ callback }),
      });
      const text = await res.text();
      if (!res.ok) {
        claudeCodeNote = `error: ${text}`;
        return;
      }
      const d = JSON.parse(text) as { status: string; error?: string };
      if (d.status === "complete") {
        claudeCodeNote = "Claude Code linked";
        claudeCodeUrl = "";
        claudeCodeCallback = "";
        await refreshAuth();
        await refreshNode();
        setupNotice = "Claude Code login complete — model set to claude-code";
      } else {
        claudeCodeNote = d.error || "login failed";
      }
    } catch (err) {
      claudeCodeNote = `request failed: ${err}`;
    }
  }

  async function importGeminiCli() {
    setupNotice = "";
    try {
      const res = await fetch(`${HTTP}/auth/gemini/import-cli`, { method: "POST", headers: authHeaders() });
      const text = await res.text();
      if (!res.ok) {
        setupNotice = `error: ${text}`;
        return;
      }
      const d = JSON.parse(text) as { message: string; status: AuthStatus };
      setAuth(d.status);
      setupNotice = d.message;
      refreshNode();
    } catch (err) {
      setupNotice = `request failed: ${err}`;
    }
  }

  onMount(() => {
    refreshAuth();
    refreshProviders();
    refreshCliAccounts();
    refreshClaudeNative();
    void refreshProxy();
    void refreshKeys();
  });
  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
  });
</script>

<!-- Settings rows (settings.css): each account is its name and state on the
     left, its sign-in on the right, and any sign-in flow opens underneath.
     Then the node-wide model and key, then the expert adapters behind a
     disclosure. `li.row` / `.chip` / `details.advanced` are also what the
     provider-accounts spec reads. -->
<div class="set-panel">
  <p class="set-lead">
    Connect the accounts that power Bakery agents. Sign-in opens the provider's own browser flow;
    Bakery reports <strong>linked / API key / not linked</strong> only and never displays a secret
    value.
  </p>

  <section class="set-section">
    <ul class="set-rows">
      {#each PROVIDERS as p (p.key)}
        {@const s = statusOf(p.key)}
        {@const chip = chipFor(s)}
        <li class="row set-row">
          <span class="set-row-lead">
            <span class="initial" style="background: {p.tint}" aria-hidden="true">{p.initial}</span>
            <span class="set-row-text">
              <span class="set-row-title">{p.name}</span>
              <span class="set-row-desc">{detailFor(s)}</span>
            </span>
          </span>

          <span class="set-row-control">
            <span class="chip set-chip {chip.cls}">{chip.text}</span>
            {#if p.key === "xai"}
              <button type="button" class="set-btn" onclick={startXaiLogin}>
                {s?.oauth_set ? "Re-authorize" : "Log in with Grok account…"}
              </button>
            {:else if p.key === "gemini"}
              <button type="button" class="set-btn" onclick={importGeminiCli}>Import CLI login</button>
            {:else if p.key === "anthropic"}
              <button type="button" class="set-btn" onclick={startClaudeCodeLogin}>
                {s?.oauth_set ? "Re-authorize Claude Code" : "Sign in with Claude Code"}
              </button>
            {/if}
            <button
              type="button"
              class="set-btn danger"
              disabled={!s?.oauth_set && !s?.api_key_set}
              onclick={() => clearProvider(p.key)}
            >
              Clear
            </button>
          </span>

          {#if p.key === "anthropic" && (claudeCodeUrl || claudeCodeNote)}
            <div class="set-row-more device">
              {#if claudeCodeUrl}
                <p class="device-line">
                  <a href={claudeCodeUrl} target="_blank" rel="noreferrer">Open Claude sign-in</a>
                </p>
                <div class="device-form">
                  <input
                    class="set-input"
                    placeholder="paste callback URL or code"
                    bind:value={claudeCodeCallback}
                    autocomplete="off"
                    spellcheck="false"
                  />
                  <button type="button" class="set-btn" disabled={!claudeCodeCallback.trim()} onclick={completeClaudeCodeLogin}>
                    Finish sign-in
                  </button>
                </div>
              {/if}
              <p class="poll-note">{claudeCodeNote}</p>
            </div>
          {/if}

          {#if p.key === "xai" && xaiUserCode}
            <div class="set-row-more device">
              <p class="device-line">
                Code <code class="user-code">{xaiUserCode}</code>
                {#if xaiVerifyUrl}
                  <a href={xaiVerifyUrl} target="_blank" rel="noreferrer">Open xAI verify</a>
                {/if}
              </p>
              <p class="poll-note">{xaiPollNote}</p>
            </div>
          {/if}
        </li>
      {/each}

      <li class="row set-row" data-testid="claude-native-account">
        <span class="set-row-lead">
          <span class="initial" style="background: var(--violet)" aria-hidden="true">C</span>
          <span class="set-row-text">
            <span class="set-row-title">Claude native agent</span>
            <span class="set-row-desc">
              {#if claudeNative.connected}
                {claudeNative.label || "Claude subscription"} · ACP adapter {claudeNative.adapter_version}
              {:else}
                {claudeNative.reason || `Pinned ACP adapter ${claudeNative.adapter_version}`}
              {/if}
            </span>
          </span>
        </span>

        <span class="set-row-control">
          <span class="chip set-chip {claudeNativeChip().cls}">{claudeNativeChip().text}</span>
          <button
            type="button"
            class="set-btn"
            disabled={!claudeNative.installed || !!claudeNativeTerminalId}
            onclick={signInClaudeNative}
          >
            {claudeNative.connected ? "Re-authorize Claude subscription" : "Sign in with Claude subscription"}
          </button>
        </span>

        {#if claudeNativeTerminalId}
          <div class="set-row-more device cli-login-device">
            <p class="poll-note">
              Complete Claude's native browser sign-in below. Bakery verifies subscription status after it exits.
            </p>
            <div class="cli-login-term">
              <Terminal
                streamId={SYSTEM_ACCOUNTS_STREAM_ID}
                terminalId={claudeNativeTerminalId}
                name="claude-native-login"
                onExited={onClaudeNativeLoginExited}
                onClosed={() => void onClaudeNativeLoginExited(null)}
              />
            </div>
          </div>
        {/if}
      </li>

      {#each CLI_TOOLS as t (t.key)}
        {@const s = cli[t.key]}
        {@const chip = cliChipFor(s)}
        <li class="row set-row">
          <span class="set-row-lead">
            <span class="initial" style="background: {t.tint}" aria-hidden="true">{t.initial}</span>
            <span class="set-row-text">
              <span class="set-row-title">{t.name}</span>
              <span class="set-row-desc">
                {#if s.linked}account login{:else if s.installed}no credential on this node{:else}not installed on this node{/if}
              </span>
            </span>
          </span>

          <span class="set-row-control">
            <span class="chip set-chip {chip.cls}">{chip.text}</span>
            <button
              type="button"
              class="set-btn"
              disabled={cliLoginTool === t.key}
              onclick={() => signInCli(t.key, t.command)}
            >
              {s.linked ? "Re-authorize" : `Sign in with ${t.name}`}
            </button>
          </span>

          {#if cliLoginTool === t.key && cliLoginTerminalId}
            <div class="set-row-more device cli-login-device">
              <p class="poll-note">
                Finish signing in below — this closes on its own once {t.name}'s login completes.
              </p>
              <div class="cli-login-term">
                <Terminal
                  streamId={SYSTEM_ACCOUNTS_STREAM_ID}
                  terminalId={cliLoginTerminalId}
                  name={`${t.key}-login`}
                  onExited={(code) => onCliLoginExited(t.key, code)}
                  onClosed={() => void onCliLoginExited(t.key, null)}
                />
              </div>
            </div>
          {/if}
        </li>
      {/each}
    </ul>

    {#if cliNotice}<p class="set-status">{cliNotice}</p>{/if}
  </section>

  <section class="set-section">
    <form class="set-row" onsubmit={setModel}>
      <span class="set-row-text">
        <label class="set-row-title" for="model-pick">Active model</label>
        {#if auth}
          <span class="set-row-desc">active model · <code>{auth.resolved_hint}</code></span>
        {/if}
      </span>
      <span class="set-row-control">
        <select id="model-pick" class="set-select" bind:value={modelPick}>
          <option value="auto">Automatic</option>
          <option value="grok">Grok</option>
          <option value="gemini">Gemini</option>
          <option value="claude">Claude (API key)</option>
          <option value="claude-code">Claude Code subscription</option>
          <option value="stub">Offline test model</option>
        </select>
        <button type="submit" class="set-btn">Use model</button>
      </span>
    </form>

    <form class="set-row" onsubmit={saveApiKey}>
      <span class="set-row-text">
        <span class="set-row-title">API key</span>
        <span class="set-row-desc">
          The key is posted to the node and cleared from this field immediately. It is never read back —
          the node only ever reports whether one is set.
        </span>
      </span>
      <span class="set-row-control">
        <select class="set-select" bind:value={keyProvider} aria-label="API key provider">
          {#each PROVIDERS as p (p.key)}
            <option value={p.key}>{p.keyLabel}</option>
          {/each}
        </select>
        <input
          class="set-input key-input"
          type="password"
          placeholder="paste API key"
          bind:value={keyInput}
          autocomplete="off"
          spellcheck="false"
        />
        <button type="submit" class="set-btn primary" disabled={!keyInput.trim()}>Save key</button>
      </span>
    </form>

    {#if setupNotice}<p class="set-status">{setupNotice}</p>{/if}
  </section>

  <section class="set-section" data-testid="model-proxy">
    <h2 class="set-section-h">Model proxy</h2>
    <p class="set-muted">
      One door for model calls made with an API key, on this Mac only. Keys stay with the proxy; Bakery never shows
      one.
    </p>
    {#if proxy}
      <div class="set-actions">
        {#if !proxy.installed}
          <button type="button" class="set-btn" onclick={() => void installProxy()} disabled={installing}>
            {installing ? "Installing…" : "Install the model proxy"}
          </button>
          <span class="set-muted">CLIProxyAPI {proxy.version}, its download checked against the pinned sha256.</span>
        {:else if proxy.healthy}
          <span class="set-chip ok">running</span>
          <span class="set-muted">CLIProxyAPI {proxy.version} on 127.0.0.1:{proxy.port}</span>
        {:else}
          <span class="set-chip warn">{proxy.running ? "starting" : "stopped"}</span>
          {#if proxy.error}<span class="set-muted">{proxy.error}</span>{/if}
          <button type="button" class="set-btn sm" onclick={() => void startProxy()}>Start it</button>
          <button type="button" class="set-btn sm" onclick={() => void refreshProxy()}>Check again</button>
        {/if}
      </div>
      {#if proxy.installed}
        <h3 class="set-subhead">Model keys</h3>
        <ul class="set-rows" data-testid="model-keys">
          {#each KEY_PROVIDERS as p (p.id)}
            {@const keys = modelKeys?.providers.find((x) => x.provider === p.id)?.keys ?? []}
            <li class="set-row">
              <span class="set-row-text">
                <span class="set-row-title">{p.name}</span>
                <span class="set-row-desc">
                  {keys.length === 0 ? "no key: calls go to the provider directly" : `${keys.length} key${keys.length === 1 ? "" : "s"}`}
                </span>
              </span>
            </li>
            {#each keys as k (k.id)}
              <li class="set-row">
                <span class="set-row-text">
                  <span class="set-row-title">{k.label}</span>
                  <span class="set-row-desc">added {k.added.slice(0, 10)}</span>
                </span>
                <button
                  type="button"
                  class="set-btn sm danger"
                  disabled={keyBusy}
                  onclick={() => void removeKey(p.id, k.id, k.label)}
                  aria-label="Remove {k.label}">Remove</button
                >
              </li>
            {/each}
          {/each}
        </ul>
        <form class="set-actions" onsubmit={(e) => { e.preventDefault(); void addKey(); }}>
          <select class="set-select" bind:value={newKeyProvider} aria-label="Provider">
            {#each KEY_PROVIDERS as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
          </select>
          <input class="set-input" bind:value={newKeyLabel} placeholder="Label (optional)" aria-label="Key label" />
          <input
            class="set-input"
            type="password"
            autocomplete="off"
            bind:value={newKeyText}
            placeholder="API key"
            aria-label="API key"
          />
          <button type="submit" class="set-btn" disabled={keyBusy || !newKeyText.trim() || !proxy.healthy}>Add key</button>
        </form>
        <p class="set-muted">The key goes to the proxy only; Bakery never shows it again.</p>
      {/if}
    {/if}
    {#if proxyError}<p class="set-error">{proxyError}</p>{/if}
  </section>

  <section class="set-section">
    <details class="advanced set-disclosure heading">
      <summary>Advanced adapters</summary>
      <div class="advanced-body">
        <p class="set-section-desc">
          Expert configuration for custom ACP processes and OpenAI-compatible endpoints. Normal
          provider accounts above never require an id, argv, environment variable, or config path.
          Adapter secrets are write-only: this screen reports only whether one is set.
        </p>

        <div class="plug-group">
          <div class="set-section-hd">
            <h3 class="set-subhead">ACP CLIs</h3>
            <button type="button" class="set-btn sm" onclick={openAddAcpForm}>+ Add ACP plug</button>
          </div>
          {#if acpPlugs.length === 0}
            <p class="set-muted">No named ACP plugs configured on this node.</p>
          {:else}
            <ul class="set-rows">
              {#each acpPlugs as plug (plug.id)}
                {@const ready = readiness.get(plug.id)}
                <li class="row set-row">
                  <span class="set-row-text">
                    <span class="set-row-title">{plug.label || plug.id}</span>
                    <span class="set-row-desc">
                      <code>{plug.id}</code> · argv: <code>{plug.argv.join(" ")}</code>
                      {#if ready && !ready.node_available && ready.reason}
                        — {ready.reason}
                      {/if}
                    </span>
                  </span>
                  <span class="set-row-control">
                    <span class="chip set-chip {ready?.node_available ? 'ok' : ''}">
                      {ready?.node_available ? "Ready" : "Setup needed"}
                    </span>
                    <button type="button" class="set-btn sm" onclick={() => openEditAcpForm(plug)}>Edit</button>
                    <button type="button" class="set-btn sm danger" onclick={() => deleteAcpPlug(plug.id)}>Remove</button>
                  </span>
                </li>
              {/each}
            </ul>
          {/if}

          {#if showAcpForm}
            <form class="plug-form" onsubmit={submitAcpForm}>
              <div class="set-field">
                <label for="acp-id">id</label>
                <input id="acp-id" class="set-input mono" bind:value={acpFormId} placeholder="acp:codex" disabled={!!editingAcpId} />
              </div>
              <div class="set-field">
                <label for="acp-label">label</label>
                <input id="acp-label" class="set-input" bind:value={acpFormLabel} placeholder="Codex CLI (optional)" />
              </div>
              <div class="set-field">
                <label for="acp-argv">argv</label>
                <input id="acp-argv" class="set-input mono" bind:value={acpFormArgv} placeholder="cursor-agent acp" />
              </div>
              <div class="set-field">
                <span class="set-field-label">Examples</span>
                <span class="examples">
                  <button type="button" class="set-btn sm" onclick={() => fillAcpExample("cursor")}>cursor-agent acp</button>
                  <button type="button" class="set-btn sm" onclick={() => fillAcpExample("agy")}>agy-acp</button>
                  <button type="button" class="set-btn sm" onclick={() => fillAcpExample("grok")}>grok agent stdio</button>
                </span>
              </div>
              <div class="set-field">
                <label for="acp-env">env</label>
                <textarea
                  id="acp-env"
                  class="set-input"
                  bind:value={acpFormEnv}
                  placeholder={"AGY_BIN=/path/to/agy\nCODEX_HOME=/path/to/codex-home"}
                  rows="2"
                ></textarea>
              </div>
              <div class="set-field">
                <label for="acp-cfg-src">config source</label>
                <input id="acp-cfg-src" class="set-input mono" bind:value={acpFormConfigSource} placeholder="/path/to/config-dir (optional)" />
              </div>
              <div class="set-field">
                <label for="acp-cfg-dest">config dest</label>
                <input id="acp-cfg-dest" class="set-input mono" bind:value={acpFormConfigDest} placeholder=".config/codex (optional)" />
              </div>
              <div class="set-actions">
                <button type="submit" class="set-btn primary">{editingAcpId ? "Save" : "Add"}</button>
                <button type="button" class="set-btn" onclick={() => (showAcpForm = false)}>Cancel</button>
              </div>
            </form>
          {/if}
        </div>

        <div class="plug-group">
          <div class="set-section-hd">
            <h3 class="set-subhead">Openai-compat local models</h3>
            <button type="button" class="set-btn sm" onclick={openAddOcForm}>+ Add openai-compat plug</button>
          </div>
          {#if ocPlugs.length === 0}
            <p class="set-muted">No named openai-compat plugs configured on this node.</p>
          {:else}
            <ul class="set-rows">
              {#each ocPlugs as plug (plug.id)}
                {@const ready = readiness.get(plug.id)}
                <li class="row set-row">
                  <span class="set-row-text">
                    <span class="set-row-title">{plug.label || plug.id}</span>
                    <span class="set-row-desc">
                      <code>{plug.id}</code> · {plug.base_url} · {plug.model}
                      {#if ready && !ready.node_available && ready.reason}
                        — {ready.reason}
                      {/if}
                    </span>
                  </span>
                  <span class="set-row-control">
                    <span class="chip set-chip {ready?.node_available ? 'ok' : ''}">
                      {ready?.node_available ? "Ready" : "Setup needed"}
                    </span>
                    <button type="button" class="set-btn sm" onclick={() => openEditOcForm(plug)}>Edit</button>
                    <button type="button" class="set-btn sm danger" onclick={() => deleteOcPlug(plug.id)}>Remove</button>
                  </span>
                  <div class="set-row-more token-form">
                    <input
                      class="set-input"
                      type="password"
                      placeholder="bearer token (optional)"
                      value={plugSecretInput[plug.id] ?? ""}
                      oninput={(e) => (plugSecretInput[plug.id] = (e.target as HTMLInputElement).value)}
                      autocomplete="off"
                      spellcheck="false"
                    />
                    <button
                      type="button"
                      class="set-btn sm"
                      disabled={!(plugSecretInput[plug.id] ?? "").trim()}
                      onclick={() => savePlugSecret(plug.id)}
                    >
                      Save token
                    </button>
                    <button type="button" class="set-btn sm" onclick={() => clearPlugSecret(plug.id)}>Clear token</button>
                  </div>
                </li>
              {/each}
            </ul>
          {/if}

          {#if showOcForm}
            <form class="plug-form" onsubmit={submitOcForm}>
              <div class="set-field">
                <label for="oc-id">id</label>
                <input id="oc-id" class="set-input mono" bind:value={ocFormId} placeholder="local:ollama" disabled={!!editingOcId} />
              </div>
              <div class="set-field">
                <label for="oc-label">label</label>
                <input id="oc-label" class="set-input" bind:value={ocFormLabel} placeholder="Local Ollama (optional)" />
              </div>
              <div class="set-field">
                <label for="oc-url">base_url</label>
                <input id="oc-url" class="set-input mono" bind:value={ocFormBaseUrl} placeholder="http://127.0.0.1:11434/v1" />
              </div>
              <div class="set-field">
                <label for="oc-model">model</label>
                <input id="oc-model" class="set-input mono" bind:value={ocFormModel} placeholder="llama3" />
              </div>
              <div class="set-actions">
                <button type="submit" class="set-btn primary">{editingOcId ? "Save" : "Add"}</button>
                <button type="button" class="set-btn" onclick={() => (showOcForm = false)}>Cancel</button>
              </div>
            </form>
          {/if}
        </div>

        {#if plugNotice}<p class="set-status">{plugNotice}</p>{/if}
      </div>
    </details>
  </section>
</div>

<style>
  /* Rows, controls and text come from the shared settings vocabulary; only
     what is particular to accounts lives here. */
  .initial {
    flex: 0 0 auto;
    width: 30px;
    height: 30px;
    border-radius: var(--r-lg);
    display: flex;
    align-items: center;
    justify-content: center;
    font: 600 13px/1 var(--font-ui);
    color: var(--fg-on-tint);
  }

  /* A sign-in in progress opens under its row, aligned with the row text. */
  .set-row .device {
    margin-left: 42px;
    padding: 10px 12px;
    border-radius: var(--r-lg);
    background: var(--bg-panel);
  }
  .device-line {
    margin: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font: var(--set-font-desc);
    color: var(--fg-dim);
  }
  .device-form {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .device-form .set-input { flex: 1 1 220px; }
  .user-code {
    font: 600 15px/1.2 var(--font-mono);
    letter-spacing: 0.08em;
    color: var(--fg);
  }
  .poll-note {
    margin: 0;
    font: 400 11.5px/1.45 var(--font-mono);
    color: var(--fg-faint);
    overflow-wrap: anywhere;
  }
  .poll-note:empty { display: none; }
  .cli-login-term {
    height: 320px;
    border-radius: var(--r-lg);
    overflow: hidden;
    border: 1px solid var(--line-soft);
  }

  .key-input { width: 200px; }

  .advanced-body {
    display: flex;
    flex-direction: column;
    gap: 22px;
    margin-top: 8px;
  }
  .plug-group {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .plug-group .set-subhead { margin: 0; }
  .plug-form {
    margin-top: 6px;
    padding-top: 6px;
    border-top: 1px solid var(--line-soft);
  }
  .examples {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .set-row .token-form {
    flex-direction: row;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .token-form .set-input { flex: 1 1 200px; }

  @container settings (max-width: 520px) {
    .set-row .device { margin-left: 0; }
    .key-input { width: auto; flex: 1 1 160px; }
  }
</style>

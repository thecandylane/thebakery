<script lang="ts">
  import { CUSTOM_TOKEN_KEYS, isColor, type BuiltinTheme, type CustomTokens } from "$lib/theme";
  let { base, tokens = {}, large = false }: { base: BuiltinTheme; tokens?: CustomTokens; large?: boolean } = $props();
  const overrides = $derived(CUSTOM_TOKEN_KEYS.filter((key) => isColor(tokens[key]))
    .map((key) => `${key}:${tokens[key]}`).join(";"));
</script>

<div class="scheme-preview" class:large data-palette={base} style={overrides} aria-hidden="true">
  <div class="preview-rail">
    <span class="preview-brand">B</span>
    <span class="rail-line wide"></span><span class="rail-line active"></span>
    <span class="rail-line"></span><span class="rail-line short"></span><span class="rail-footer"></span>
  </div>
  <div class="preview-workspace">
    <div class="preview-header"><span>Studio</span><span class="status-dot"></span></div>
    <div class="preview-content">
      <span class="preview-title">Make it yours.</span>
      <span class="prose-line"></span><span class="prose-line short"></span>
      <div class="preview-code"><span class="keyword">const</span> studio <span class="muted">=</span> <span class="string">"Bakery"</span></div>
      <div class="preview-composer"><span>Ask Baker anything…</span><span class="send">↑</span></div>
    </div>
  </div>
</div>

<style>
  .scheme-preview { display: flex; height: 118px; overflow: hidden; isolation: isolate; border: 1px solid var(--line); border-radius: 8px; background: var(--bg-app); color: var(--fg); text-align: left; font: 500 8px/1.4 var(--font-ui); user-select: none; pointer-events: none; }
  .preview-rail { width: 25%; min-width: 38px; display: flex; flex-direction: column; gap: 7px; padding: 10px 7px; background: var(--bg-chrome); border-right: 1px solid var(--line-chrome); }
  .preview-brand { width: 16px; height: 16px; display: grid; place-content: center; color: var(--fg-on-solid); background: var(--baker-solid); border-radius: 5px; margin-bottom: 2px; }
  .rail-line { height: 3px; width: 80%; background: var(--fg-fainter); opacity: .55; border-radius: 2px; }
  .rail-line.active { width: 100%; height: 9px; margin: -2px 0; opacity: 1; background: var(--accent-bg-2); border-left: 2px solid var(--accent); }
  .rail-line.short { width: 55%; }
  .rail-line.wide { width: 92%; }
  .rail-footer { margin-top: auto; height: 5px; width: 50%; border-radius: 2px; background: var(--fg-faint); }
  .preview-workspace { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .preview-header { height: 23px; display: flex; align-items: center; justify-content: space-between; padding: 0 10px; background: var(--bg-panel); color: var(--fg-dim); border-bottom: 1px solid var(--line-chrome); }
  .status-dot { width: 4px; height: 4px; background: var(--ok); border-radius: 50%; }
  .preview-content { flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 5px; padding: 9px 10px 7px; }
  .preview-title { font-size: 9px; font-weight: 600; }
  .prose-line { height: 3px; width: 88%; background: var(--fg-dim); opacity: .55; border-radius: 2px; }
  .prose-line.short { width: 63%; }
  .preview-code { display: none; padding: 7px; border-radius: 5px; border: 1px solid var(--line-soft); background: var(--bg-rail); font-family: var(--font-mono); }
  .keyword { color: var(--syntax-keyword); }
  .string { color: var(--syntax-string); }
  .muted { color: var(--syntax-comment); }
  .preview-composer { min-height: 23px; display: flex; align-items: center; justify-content: space-between; gap: 5px; margin-top: auto; padding: 3px 5px 3px 7px; border: 1px solid var(--line); border-radius: 6px; background: var(--bg-panel); color: var(--fg-dim); font-size: 7px; }
  .preview-composer > span:first-child { overflow: hidden; white-space: nowrap; }
  .send { width: 16px; height: 16px; flex: 0 0 auto; display: grid; place-items: center; border-radius: 5px; background: var(--accent-solid); color: var(--fg-on-solid); font-size: 11px; }
  .large { height: 225px; font-size: 11px; }
  .large .preview-rail { width: 23%; padding: 17px 12px; gap: 12px; }
  .large .preview-brand { width: 23px; height: 23px; font-size: 12px; margin-bottom: 9px; }
  .large .preview-header { height: 31px; padding: 0 16px; }
  .large .preview-content { padding: 17px 16px 12px; gap: 10px; }
  .large .preview-title { font-size: 16px; }
  .large .preview-code { display: block; }
  .large .preview-composer { min-height: 32px; font-size: 10px; padding: 4px 7px 4px 10px; }
  .large .send { width: 24px; height: 24px; font-size: 15px; }
</style>

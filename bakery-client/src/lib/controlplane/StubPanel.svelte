<script lang="ts">
  // PANELS.md: "Every panel opens with a 15px/600 heading and an 11.5px/1.5
  // --fg-faint subhead of one sentence that states what the panel is for —
  // not what it contains." The subhead text below is lifted from PANELS.md
  // verbatim where it exists; the note line is the honest "not built yet"
  // this task requires instead of inventing data.

  let {
    heading,
    subhead,
    note = "Not recorded yet — this panel is wired but has no live data source on the node yet.",
  }: {
    heading: string;
    subhead: string;
    note?: string;
  } = $props();
</script>

<div class="stub-panel">
  <h1>{heading}</h1>
  <p class="subhead">{subhead}</p>
  <p class="note">{note}</p>
</div>

<style>
  /* Only shown for an address with no page (the Settings frame then shows
     no title of its own), so it carries the frame's title and lead type. */
  .stub-panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  h1 {
    margin: 0;
    font: var(--set-font-title, 600 20px/1.3 var(--font-ui));
    color: var(--fg);
  }
  .subhead {
    margin: 0;
    font: var(--set-font-body, 400 13px/1.5 var(--font-ui));
    color: var(--fg-dim);
    max-width: 620px;
  }
  .note {
    margin: 20px 0 0;
    padding: 10px 12px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-lg);
    background: var(--bg-panel);
    font: var(--set-font-desc, 400 12px/1.5 var(--font-ui));
    color: var(--fg-dim);
    max-width: 620px;
  }
</style>

<script lang="ts">
  import { onDestroy } from "svelte";

  // Split handle. unit "pct" is a percentage of the *parent container*
  // (studio or sidebar column), not the window — so the handle tracks the
  // cursor when rail/sidebar take space. Snapshot parent size on mousedown
  // only (no getBoundingClientRect per mousemove).

  let {
    axis,
    value,
    min,
    max,
    unit,
    onchange,
    oncommit,
  }: {
    axis: "x" | "y";
    value: number;
    min: number;
    max: number;
    unit: "px" | "pct";
    onchange: (v: number) => void;
    oncommit: (v: number) => void;
  } = $props();

  let el: HTMLDivElement | undefined = $state();
  let startCoord = 0;
  let startValue = 0;
  /** Parent size along the drag axis, captured at mousedown (px). */
  let startTotal = 0;
  let prevBodyCursor = "";

  function clamp(v: number): number {
    if (!Number.isFinite(v)) return Number.isFinite(min) ? min : 0;
    return Math.max(min, Math.min(max, v));
  }

  function compute(clientX: number, clientY: number): number {
    const coord = axis === "x" ? clientX : clientY;
    const delta = coord - startCoord;
    if (unit === "pct") {
      if (!startTotal) return clamp(startValue);
      return clamp(startValue + (delta / startTotal) * 100);
    }
    return clamp(startValue + delta);
  }

  function onMouseMove(e: MouseEvent) {
    onchange(compute(e.clientX, e.clientY));
  }

  function endDrag() {
    window.removeEventListener("mousemove", onMouseMove);
    window.removeEventListener("mouseup", onMouseUp);
    document.body.style.cursor = prevBodyCursor;
  }

  function onMouseUp(e: MouseEvent) {
    endDrag();
    oncommit(compute(e.clientX, e.clientY));
  }

  function onMouseDown(e: MouseEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    startCoord = axis === "x" ? e.clientX : e.clientY;
    startValue = value;
    const r = el?.parentElement?.getBoundingClientRect();
    startTotal = axis === "x" ? (r?.width ?? 0) : (r?.height ?? 0);
    prevBodyCursor = document.body.style.cursor;
    document.body.style.cursor = axis === "x" ? "col-resize" : "row-resize";
    window.addEventListener("mousemove", onMouseMove);
    window.addEventListener("mouseup", onMouseUp);
  }

  function onKeyDown(e: KeyboardEvent) {
    const decrease = axis === "x" ? e.key === "ArrowLeft" : e.key === "ArrowUp";
    const increase = axis === "x" ? e.key === "ArrowRight" : e.key === "ArrowDown";
    let next: number | null = null;
    if (decrease || increase) {
      const step = (unit === "px" ? 8 : 1) * (e.shiftKey ? 4 : 1);
      next = clamp(value + (increase ? step : -step));
    } else if (e.key === "Home") {
      next = min;
    } else if (e.key === "End") {
      next = max;
    }
    if (next == null) return;
    e.preventDefault();
    onchange(next);
    oncommit(next);
  }

  onDestroy(endDrag);
</script>

<!-- An adjustable separator is intentionally focusable and interactive per
     the ARIA separator pattern; Svelte's static rule treats every div as
     non-interactive even once role="separator" is present. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  bind:this={el}
  class="resizer"
  class:x={axis === "x"}
  class:y={axis === "y"}
  role="separator"
  tabindex="0"
  aria-orientation={axis === "x" ? "vertical" : "horizontal"}
  aria-label={axis === "x" ? "Resize sidebar" : "Resize panel"}
  aria-valuemin={min}
  aria-valuemax={max}
  aria-valuenow={value}
  onmousedown={onMouseDown}
  onkeydown={onKeyDown}
></div>

<style>
  .resizer {
    position: relative;
    z-index: 4;
    flex: 0 0 auto;
    background: transparent;
    border: 0;
    padding: 0;
    /* no transitions — drag must be immediate */
  }
  .resizer.x {
    width: 4px;
    align-self: stretch;
    cursor: col-resize;
  }
  .resizer.y {
    height: 4px;
    align-self: stretch;
    cursor: row-resize;
  }
  .resizer:hover {
    background: var(--accent-border);
  }
  .resizer:focus-visible {
    outline: none;
    background: var(--accent);
    box-shadow: 0 0 0 1px var(--bg-app), 0 0 0 3px var(--accent-border);
  }
  .resizer::after {
    content: "";
    position: absolute;
    inset: -5px;
  }
</style>

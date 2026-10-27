export type CompactOverlayOwner =
  | "primary-navigation"
  | "command-palette"
  | "settings-navigation";

const COMPACT_OVERLAY_CLAIM = "bakery:compact-overlay-claim";

/**
 * Compact drawers are rendered by different route shells, but only one may
 * own keyboard focus at a time. Claiming the overlay lane synchronously asks
 * every other owner to close before the new surface moves focus into itself.
 */
export function claimCompactOverlay(owner: CompactOverlayOwner): void {
  window.dispatchEvent(
    new CustomEvent<CompactOverlayOwner>(COMPACT_OVERLAY_CLAIM, { detail: owner }),
  );
}

export function onCompactOverlayClaim(
  listener: (owner: CompactOverlayOwner) => void,
): () => void {
  const handle = (event: Event) => {
    listener((event as CustomEvent<CompactOverlayOwner>).detail);
  };
  window.addEventListener(COMPACT_OVERLAY_CLAIM, handle);
  return () => window.removeEventListener(COMPACT_OVERLAY_CLAIM, handle);
}

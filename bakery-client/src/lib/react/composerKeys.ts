// React carries a key event up the component tree, past a portal, so a key
// pressed in a dialog the composer opened still reaches the composer's own
// handler. The DOM does not: the real target sits outside the composer's
// element. This is the test the composer's shortcuts run first, kept free of
// React so `composerKeys.check.ts` runs it as the composer does.

/** True when the key event started inside `root` (the composer itself), not
 *  in a dialog or popover that React bubbled up from a portal. */
export function fromComposer(root: { contains(node: unknown): boolean }, target: unknown): boolean {
  return root.contains(target);
}

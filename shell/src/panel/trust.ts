const GUARDED = ["click", "keydown", "input", "submit", "pointerdown"] as const;

/**
 * The page can dispatch events into the panel's tree; a person cannot be
 * impersonated that way. Anything not `isTrusted` is stopped in the capture
 * phase before it reaches a panel listener (threat model section 7).
 */
export function dropUntrusted(root: ShadowRoot): void {
  for (const type of GUARDED) {
    root.addEventListener(
      type,
      (event) => {
        if (!event.isTrusted) {
          event.stopImmediatePropagation();
          event.preventDefault();
        }
      },
      true,
    );
  }
}

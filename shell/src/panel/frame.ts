/** Only the top frame gets a panel: the sidecar's script runs in every frame. */
export function shouldMount(win: Window): boolean {
  try {
    return win.top === win.self;
  } catch {
    // A top we cannot even compare is not ours either.
    return false;
  }
}

/**
 * The app declares every token on `:root`, which matches nothing inside a shadow
 * root: left as it was, no variable reached the panel and it drew in the
 * browser's bare defaults. On the shadow host they cascade into the panel.
 */
export function hostScoped(css: string): string {
  return css.replace(/:root\b/g, ":host");
}

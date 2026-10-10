/** Only the top frame gets a panel: the sidecar's script runs in every frame. */
export function shouldMount(win: Window): boolean {
  try {
    return win.top === win.self;
  } catch {
    // A top we cannot even compare is not ours either.
    return false;
  }
}

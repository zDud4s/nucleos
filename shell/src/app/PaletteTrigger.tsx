import { Search } from "lucide-react";
import { SHORTCUT_HINT, usePaletteOpen } from "../ui";

/**
 * The visible way into the palette, for whoever does not know the chord.
 *
 * A rail row rather than a page control, because the palette is not a feature
 * of any page — it is the fastest way to leave the one you are on. It sits in
 * the footer, which is pinned: on screen at every scroll position of the rail
 * above it, on every route.
 *
 * Shaped and measured like `NotificationsDrawer`'s own trigger and like every
 * destination above it — same mark, same gap, same insets — because it is one
 * more place to go, and a control that reads as a button in a list of rows
 * reads as a different kind of thing.
 */
export function PaletteTrigger() {
  const open = usePaletteOpen();
  /*
    Spelled out, because the chord beside the words would otherwise be announced
    as "Go to…Ctrl K" — adjacent inline text concatenates with no separator.
    The same call `spokenName` makes for every badged row in the rail.
  */
  const spoken = `Go to anything, ${SHORTCUT_HINT}`;

  return (
    <button type="button" className="app-goto" aria-label={spoken} title={spoken} onClick={open}>
      <Search className="app-goto-glyph" strokeWidth={1.5} aria-hidden="true" />
      <span className="app-goto-label">Go to…</span>
      {/* The chord is shown and not only bound: a shortcut nobody is told about
          is a shortcut for whoever already knew. */}
      <kbd className="app-goto-keys">{SHORTCUT_HINT}</kbd>
    </button>
  );
}

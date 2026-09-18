import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "./vendor/command";

/** One row of the palette. */
export interface PaletteItem {
  /**
   * Stable within the whole palette, not only within its group — cmdk keys its
   * own selection by this value, so two groups spending the same id would fight
   * over which row is highlighted.
   */
  id: string;
  /** What is drawn. A page may spend its own classes here. */
  label: ReactNode;
  /**
   * What is matched, and the reason this is a field rather than a read of the
   * label: a label is a node, and a page that draws a row out of three spans
   * would otherwise be searchable by whichever of them happened to be a string.
   */
  match: string;
  /** Said on the right of the row, and as the row's `title`. */
  hint?: string;
  /** Listed and not hidden — see `run`. */
  disabled?: boolean;
  /**
   * What choosing the row does. The provider closes the palette and clears the
   * query *before* calling this, so a `run` that navigates never leaves a
   * dialog standing over the page it just opened.
   */
  run: () => void;
}

/** A block of rows under one heading, contributed by whoever is on screen. */
export interface PaletteGroup {
  /** The registry key. Registering a second group under the same id replaces the first. */
  id: string;
  /** The small-caps line above the rows. */
  heading: string;
  items: PaletteItem[];
  /**
   * These rows were matched by somebody else — the daemon, over text this list
   * does not hold — and are passed through untouched. Filtering them here
   * would throw away every hit whose own visible row does not contain the
   * query, which is most of them.
   */
  prematched?: boolean;
}

/**
 * `⌘K` on a Mac keyboard, `Ctrl K` everywhere else.
 *
 * Pure, and given the platform rather than reading it, so the rule is testable
 * without pretending to be another machine. The shell detects the platform
 * nowhere else: this is the only place that asks, and it asks once.
 */
export function shortcutHint(platform: string): string {
  return /mac|iphone|ipad/i.test(platform) ? "⌘K" : "Ctrl K";
}

/**
 * The chord as this machine spells it — read once, at module load.
 *
 * jsdom answers `""`, so every test sees `Ctrl K` and none of them has to
 * arrange for a platform.
 */
export const SHORTCUT_HINT = shortcutHint(
  typeof navigator === "undefined" ? "" : navigator.platform,
);

/**
 * The palette's own matching: case-insensitive substring over each item's
 * `match`, and never over its label.
 *
 * An empty query returns the very list it was given, reference and all — there
 * is nothing to narrow, and a copy would only invite a caller to believe
 * something was decided.
 */
export function matchItems(items: PaletteItem[], query: string): PaletteItem[] {
  const needle = query.trim().toLowerCase();
  if (needle === "") return items;
  return items.filter((item) => item.match.toLowerCase().includes(needle));
}

interface PaletteContextValue {
  /** Open the palette. */
  open: () => void;
  /** What is typed, for a group that must ask the daemon. */
  query: string;
  register: (group: PaletteGroup) => void;
  unregister: (id: string) => void;
}

/**
 * Outside a provider the palette is inert: opening it does nothing,
 * registrations are dropped and the query is the empty string.
 *
 * Deliberately silent rather than a throw. Half the suite mounts a page as a
 * bare fragment to assert one paragraph, and a hook that threw there would
 * turn "this page contributes rows to a palette" into a mandatory harness for
 * every test that has nothing to do with one. The real wiring is not left
 * unproven for it — `app/AppShell.test.tsx` mounts the actual shell and pins
 * the chord, the rows, the navigation and the focus return.
 */
const INERT: PaletteContextValue = {
  open: () => {},
  query: "",
  register: () => {},
  unregister: () => {},
};

const PaletteContext = createContext<PaletteContextValue>(INERT);

export interface PaletteProviderProps {
  children: ReactNode;
}

/**
 * The one palette: the chord, the open state, the query, the registry, and the
 * dialog itself.
 *
 * It renders its children *and* the dialog, so the mount site mounts one thing
 * and there is exactly one `window` keydown listener in the app. Before this
 * existed, two pages each bound `Ctrl K` to a palette of their own that could
 * not reach anywhere else — the fastest path in the app worked on two screens
 * out of twenty-eight, and did nothing on the other twenty-six.
 *
 * The registry is state private to this component and is deliberately *not* in
 * the context value. That is what stops a page looping: a page re-registering
 * on every render re-renders only the provider, never itself, so there is no
 * cycle for a freshly-built group object to run round.
 */
export function PaletteProvider({ children }: PaletteProviderProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [groups, setGroups] = useState<PaletteGroup[]>([]);

  /**
   * Keyed by `id` and replaced in place, so a group rebuilt on every render of
   * its page keeps its position in the list instead of walking to the bottom of
   * it while somebody reads.
   */
  const register = useCallback((group: PaletteGroup) => {
    setGroups((current) => {
      const at = current.findIndex((existing) => existing.id === group.id);
      if (at === -1) return [...current, group];
      const next = current.slice();
      next[at] = group;
      return next;
    });
  }, []);

  const unregister = useCallback((id: string) => {
    setGroups((current) => current.filter((group) => group.id !== id));
  }, []);

  /**
   * Whatever had the focus when the palette opened, so it can have it back.
   *
   * Radix restores focus itself, but only to a `DialogTrigger`, and this dialog
   * has none — it is opened by a chord and by a rail button, through state.
   * `react-dialog` composes `onCloseAutoFocus` as `event.preventDefault();
   * context.triggerRef.current?.focus()`, which cancels `FocusScope`'s own
   * restore to the previously focused element and then focuses `null`. So the
   * dialog unmounts, the focused row goes with it, and the caret lands on
   * `<body>` — which strands anybody navigating by keyboard at the top of the
   * page they never left. Remembered here because the one thing we may not do
   * is edit `ui/vendor/`.
   */
  const opener = useRef<HTMLElement | null>(null);

  /**
   * Closing clears the query, so opening the palette again is a fresh question
   * rather than the last one's answers under an empty box.
   */
  const change = useCallback((next: boolean) => {
    if (next) {
      const focused = document.activeElement;
      opener.current = focused instanceof HTMLElement ? focused : null;
    }
    setOpen(next);
    if (!next) setQuery("");
  }, []);

  /**
   * Focus back where it came from, once the dialog is actually gone.
   *
   * On a timeout and not inline: `FocusScope` does its own unmount pass in a
   * `setTimeout(0)` scheduled while the content is torn down, and a synchronous
   * `focus()` here would be overwritten by whatever that pass decides. Skipped
   * when the opener has left the document — a row that navigates can take its
   * own page's button with it, and focusing a detached node is a silent no-op
   * that leaves the caret on `<body>` anyway.
   */
  useEffect(() => {
    if (open) return undefined;
    const back = opener.current;
    opener.current = null;
    if (back === null || !back.isConnected) return undefined;
    const restore = setTimeout(() => back.focus(), 0);
    return () => clearTimeout(restore);
  }, [open]);

  const openPalette = useCallback(() => change(true), [change]);

  /**
   * Whether it is on screen, for the listener below to read.
   *
   * The listener toggles, so it has to know; written through a ref rather than
   * through the effect's dependencies so the app installs its one keydown
   * handler once and keeps it, instead of tearing it down and rebuilding it
   * every time somebody opens or closes the palette.
   */
  const onScreen = useRef(open);
  useEffect(() => {
    onScreen.current = open;
  }, [open]);

  /**
   * Ctrl+K, and Cmd+K for the same fingers on a Mac keyboard.
   *
   * On `window` rather than on a container because the point of it is to work
   * while the caret is in a composer, which is where it will often be.
   * `preventDefault` because Ctrl+K is a browser shortcut and the webview would
   * otherwise act on it as well.
   *
   * The one binding in the shell. A page contributes a group; it never binds
   * the chord, and `ui/Palette.test.tsx` holds that.
   *
   * It yields to a handler that already acted. cmdk's `vimBindings` default
   * binds Ctrl+K to "move the selection up", so with the dialog open one press
   * would move the highlight AND close the palette underneath it — one key
   * doing two things, one of which nobody asked for. `defaultPrevented` is the
   * browser's own record of "this was handled"; reading it is what lets the
   * inner binding win for as long as the dialog is up.
   */
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if (event.key.toLowerCase() !== "k" || !(event.ctrlKey || event.metaKey)) return;
      event.preventDefault();
      change(!onScreen.current);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [change]);

  /**
   * `groups` is not in here, and that is the loop guard described in the
   * component's docstring. `query` is: a group whose rows the daemon matches
   * has to know what was typed.
   */
  const value = useMemo<PaletteContextValue>(
    () => ({ open: openPalette, query, register, unregister }),
    [openPalette, query, register, unregister],
  );

  /**
   * Closed and cleared before the row acts. Both updates are enqueued ahead of
   * `run`, so a row that navigates cannot leave the dialog over its own
   * destination.
   */
  const select = (item: PaletteItem) => {
    change(false);
    item.run();
  };

  return (
    <PaletteContext.Provider value={value}>
      {children}
      <CommandDialog
        open={open}
        onOpenChange={change}
        title="Go to anything"
        description="Type to narrow the list. Enter opens the one highlighted."
        /* Escape closes it, and a palette is a thing you dismiss rather than
           close — the corner X is clutter that also has to be styled. */
        showCloseButton={false}
        /* The palette does its own matching: see `matchItems`, and `prematched`
           for the rows it must not match twice. */
        shouldFilter={false}
        className="ui-palette"
      >
        <CommandInput
          className="ui-palette-input"
          placeholder="Go to a page, or anything this page offers…"
          value={query}
          onValueChange={setQuery}
        />
        <CommandList className="ui-palette-list">
          <CommandEmpty className="ui-palette-empty">Nothing matches that.</CommandEmpty>
          {groups.map((group) => {
            const items =
              group.prematched === true ? group.items : matchItems(group.items, query);
            // A heading with nothing under it is a claim that this group has no
            // answer, which is not what "narrowed to none" means — and while any
            // group still draws a heading, cmdk counts the list as non-empty and
            // `CommandEmpty` never says the one sentence that is true.
            if (items.length === 0) return null;
            return (
              <CommandGroup
                key={group.id}
                className="ui-palette-group"
                heading={group.heading}
              >
                {items.map((item) => (
                  <CommandItem
                    key={item.id}
                    className="ui-palette-item"
                    value={item.id}
                    disabled={item.disabled}
                    title={item.hint}
                    onSelect={() => select(item)}
                  >
                    {item.label}
                    {item.hint === undefined ? null : (
                      <span className="ui-palette-hint">{item.hint}</span>
                    )}
                  </CommandItem>
                ))}
              </CommandGroup>
            );
          })}
        </CommandList>
      </CommandDialog>
    </PaletteContext.Provider>
  );
}

/**
 * Contribute this page's rows to the one palette, for as long as it is on
 * screen. `null` contributes nothing.
 *
 * **Memoise the group on the caller's side.** `useMemo` over whatever the rows
 * are built from is one line and it is the difference between registering once
 * and registering on every render of the page. The provider tolerates the
 * unmemoised version — a group is keyed by `id` and replaced in place, and the
 * registry is not in the context, so a page cannot drive itself round a loop —
 * but tolerating it is not the same as it being free.
 */
export function usePaletteGroup(group: PaletteGroup | null): void {
  const { register, unregister } = useContext(PaletteContext);

  useEffect(() => {
    if (group === null) return undefined;
    register(group);
    return () => unregister(group.id);
  }, [group, register, unregister]);
}

/**
 * What is typed in the palette right now, for a group whose rows somebody else
 * matches — Chats' second group is the daemon's answer over the whole text of
 * every conversation, which is text no list in the shell holds.
 */
export function usePaletteQuery(): string {
  return useContext(PaletteContext).query;
}

/** Open the palette from a control, for whoever does not know the chord. */
export function usePaletteOpen(): () => void {
  return useContext(PaletteContext).open;
}

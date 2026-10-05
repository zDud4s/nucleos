import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode, type TransitionEvent } from "react";
import { Link, useNavigate, useRouterState } from "@tanstack/react-router";
import { Check, ChevronsUpDown, LayoutList, PanelLeftClose, Plus, Search } from "lucide-react";
import { NAV, SYSTEM_ITEM, type NavBadge, type NavItem } from "./nav";
import type { AutopilotMode } from "../data/system";
import { readState } from "../ui/state-map";
import mark from "../assets/brand/mark.png";

/**
 * Where the icon-collapse lives between sessions.
 *
 * `localStorage` rather than a setting in the núcleo: this is a fact about one
 * window on one screen, not about the machine, and round-tripping it through
 * the daemon would make the rail flicker open on every cold start while the
 * first poll lands.
 */
const COLLAPSE_KEY = "nucleos.sidebar.collapsed";

/**
 * Reading and writing are both wrapped, and neither is allowed to throw.
 *
 * A storage that refuses — a locked profile, a webview started with storage
 * disabled — is a reason for the sidebar to open wide, never a reason for the
 * app to fail to render. Losing a collapse preference costs one click.
 */
function readCollapsed(): boolean {
  try {
    return window.localStorage.getItem(COLLAPSE_KEY) === "1";
  } catch {
    return false;
  }
}

function writeCollapsed(collapsed: boolean): void {
  try {
    window.localStorage.setItem(COLLAPSE_KEY, collapsed ? "1" : "0");
  } catch {
    // See above: a preference that cannot be saved is not an error worth showing.
  }
}

/**
 * Is this item the page you are on?
 *
 * Prefix-matching on the segment boundary rather than plain equality, so that
 * `/runs/412` keeps Runs lit. Equality would leave the whole rail dark the
 * moment anyone opened a detail route, which reads as "you are nowhere".
 * `/` is exempt: every path starts with it, and a prefix rule would light Home
 * on every page in the app.
 *
 * `Projects` follows the same rule and stays lit inside `/projects/alpha/state`.
 * That is intended: the rail has no project rows any more, so the one row about
 * projects is the right answer to "where am I" anywhere under it.
 */
function isActive(pathname: string, path: string): boolean {
  if (path === "/") return pathname === "/";
  return pathname === path || pathname.startsWith(`${path}/`);
}

/**
 * What the item is called out loud, badge and dot included.
 *
 * `undefined` — no `aria-label` at all — for the ordinary case, so that the
 * name stays the label in the DOM and cannot drift away from the word on
 * screen. It is only when the item carries something the eye reads and the ear
 * would not that the name is written out.
 */
function spokenName(
  label: string,
  count: number | undefined,
  noun: string,
  alert: boolean,
): string | undefined {
  const extras: string[] = [];
  if (count !== undefined) extras.push(`${count} ${noun}`);
  if (alert) extras.push("needs attention");
  return extras.length === 0 ? undefined : `${label}, ${extras.join(", ")}`;
}

const BADGE_NOUN: Record<NavBadge, string> = {
  proposals: "waiting",
  chats: "unread",
};
// The count is `open_review_items` — proposals and shadow decisions both — so the noun is items.
const PROJECT_BADGE_NOUN = "items to review";

/**
 * One project, as the switcher needs it.
 *
 * Deliberately not `ProjectSummary`: the switcher wants three fields and that
 * type has fifteen, and taking the whole thing would tie the sidebar to the
 * roster route's shape. What is here is what is drawn — the name, the dot, the
 * summons.
 */
export interface ProjectNavEntry {
  /** The daemon's project id, which is also the name it is known by. */
  id: string;
  mode: AutopilotMode;
  /** Decisions waiting in this project. Zero draws no count — a count is a summons. */
  pending: number;
}

/**
 * The project a path is inside, so the switcher names it across all its modes.
 *
 * Three segments and not two: `/projects/new` is the wizard, and a project that
 * happened to be called `new` lives at `/projects/new/state`.
 */
function projectOf(pathname: string): string | undefined {
  const parts = pathname.split("/").filter((part) => part !== "");
  return parts[0] === "projects" && parts.length >= 3 ? parts[1] : undefined;
}

/* --------------------------------------------------------------- switcher -- */

/** The path a project opens at — Estado, the mode that answers the arrival question. */
function projectPath(id: string): string {
  return `/projects/${id}/state`;
}

/** A mode in words, through the app's one state table rather than a second spelling of it. */
function modeWord(mode: AutopilotMode): string {
  return readState("autopilot", mode)?.label ?? mode;
}

/**
 * The chord that opens the switcher, as this machine spells it — the same test
 * the palette's `shortcutHint` makes for its own chord.
 */
const SWITCH_HINT = /mac|iphone|ipad/i.test(typeof navigator === "undefined" ? "" : navigator.platform)
  ? "⌘P"
  : "Ctrl P";

/**
 * Whether a key press belongs to a field somebody is typing in.
 *
 * Ctrl+P is the browser's Print, so the switcher takes it everywhere — except
 * from a field that is not its own, where an editor may have a meaning for it
 * and taking it would be the switcher reaching into somebody else's control.
 */
function typingElsewhere(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.closest(".nav-switch") !== null) return false;
  return (
    target.isContentEditable ||
    target.matches("input, textarea, select") ||
    target.closest('[contenteditable=""], [contenteditable="true"]') !== null
  );
}

/** One row of the list: the núcleo, or a project. */
interface SwitchOption {
  key: string;
  label: string;
  path: string;
  /** Absent on the núcleo, which has no mode and draws the mark instead of a dot. */
  mode?: AutopilotMode;
  pending: number;
  current: boolean;
}

/** The name with the typed text marked — first match, case-insensitive. */
function withMatch(name: string, query: string): ReactNode {
  if (query === "") return name;
  const at = name.toLowerCase().indexOf(query);
  if (at < 0) return name;
  return (
    <>
      {name.slice(0, at)}
      <mark>{name.slice(at, at + query.length)}</mark>
      {name.slice(at + query.length)}
    </>
  );
}

/**
 * The open panel, mounted only while it is open.
 *
 * Mounting fresh is what resets it: every opening starts with an empty query and
 * the first row highlighted, without an effect that has to remember to clear
 * them.
 *
 * A combobox over a listbox, and not a `role="menu"`: the input keeps focus the
 * whole time, the highlighted row is announced through `aria-activedescendant`,
 * and the rows are still links — a middle click opens one, a hover shows where
 * it goes. Hover and the arrow keys move ONE highlight, because two marks on two
 * rows ("the pointer is here, the keyboard is there") leave Enter ambiguous.
 */
function SwitcherPanel({
  id,
  projects,
  here,
  onClose,
}: {
  id: string;
  projects: ProjectNavEntry[] | undefined;
  here: string | undefined;
  /** `refocus` hands focus back to the button — Escape does, a navigation does not. */
  onClose: (refocus: boolean) => void;
}) {
  const [query, setQuery] = useState("");
  const [hot, setHot] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const navigate = useNavigate();
  const base = useId();
  const listId = `${base}-list`;
  const labelId = `${base}-label`;

  useEffect(() => {
    input.current?.focus();
  }, []);

  const typed = query.trim().toLowerCase();
  /*
    Alphabetical, not the daemon's order: this is a list somebody scans for a
    name they already know, and the daemon's order is a fact about its storage.
  */
  const matches = [...(projects ?? [])]
    .sort((a, b) => a.id.localeCompare(b.id))
    .filter((project) => project.id.toLowerCase().includes(typed));

  /*
    The núcleo first, and only while nothing is typed: it is the way back out to
    Home, not a project, and a query is a search among projects.
  */
  const options: SwitchOption[] = [
    ...(typed === ""
      ? [{ key: "nucleos", label: "NucleOS", path: "/", pending: 0, current: here === undefined }]
      : []),
    ...matches.map((project) => ({
      key: `project:${project.id}`,
      label: project.id,
      path: projectPath(project.id),
      mode: project.mode,
      pending: project.pending,
      current: project.id === here,
    })),
  ];

  const active = options.length === 0 ? -1 : Math.min(hot, options.length - 1);
  const optionId = (index: number) => `${base}-option-${index}`;

  useEffect(() => {
    if (active < 0) return;
    document.getElementById(`${base}-option-${active}`)?.scrollIntoView({ block: "nearest" });
  }, [active, base]);

  function onInputKey(event: KeyboardEvent<HTMLInputElement>) {
    const count = options.length;
    let next: number | undefined;
    if (event.key === "ArrowDown") next = count === 0 ? undefined : (active + 1) % count;
    else if (event.key === "ArrowUp") next = count === 0 ? undefined : (active - 1 + count) % count;
    else if (event.key === "Home") next = count === 0 ? undefined : 0;
    else if (event.key === "End") next = count === 0 ? undefined : count - 1;
    else if (event.key === "Enter") {
      event.preventDefault();
      if (active < 0) return;
      void navigate({ to: options[active].path });
      onClose(false);
      return;
    } else return;

    event.preventDefault();
    if (next !== undefined) setHot(next);
  }

  /*
    Escape from anywhere in the panel — the input or a footer link reached by
    Tab — and focus goes back to the button. A panel that closes and drops focus
    on `document.body` leaves a keyboard reader at the top of the page.
  */
  function onPanelKey(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    onClose(true);
  }

  function option(entry: SwitchOption, index: number) {
    const extras: string[] = [];
    if (entry.mode !== undefined) extras.push(modeWord(entry.mode));
    if (entry.pending > 0) extras.push(`${entry.pending} ${PROJECT_BADGE_NOUN}`);
    return (
      <Link
        key={entry.key}
        id={optionId(index)}
        to={entry.path}
        role="option"
        aria-selected={index === active}
        /*
          Our own answer and not the router's: from `/projects/alpha/workflows`
          the row points at `/state`, which the router does not consider current,
          and the switcher does. `exact` keeps the router from adding a second
          opinion — `/` would otherwise prefix-match every page there is.
        */
        aria-current={entry.current ? "page" : undefined}
        activeOptions={{ exact: true }}
        // The mode is a dot and the count a bare number on screen; both are words here.
        aria-label={extras.length === 0 ? undefined : `${entry.label}, ${extras.join(", ")}`}
        title={entry.mode === undefined ? undefined : modeWord(entry.mode)}
        // Out of the tab order: the input owns focus, and these are reached by arrow.
        tabIndex={-1}
        className={index === active ? "nav-switch-item nav-switch-hot" : "nav-switch-item"}
        onPointerMove={() => {
          if (index !== active) setHot(index);
        }}
        onClick={() => onClose(false)}
      >
        <span className="nav-switch-slot">
          {entry.mode === undefined ? (
            <img className="nav-switch-mini" src={mark} alt="" />
          ) : (
            <span className="nav-switch-dot" data-mode={entry.mode} aria-hidden="true" />
          )}
        </span>
        <span className="nav-switch-label">{withMatch(entry.label, typed)}</span>
        {entry.pending > 0 ? (
          <span className="nav-switch-count" aria-hidden="true">
            {entry.pending}
          </span>
        ) : null}
        {entry.current ? <Check className="nav-switch-tick" strokeWidth={2} aria-hidden="true" /> : null}
      </Link>
    );
  }

  const nucleo = typed === "" ? options[0] : undefined;
  const offset = nucleo === undefined ? 0 : 1;

  return (
    <div className="nav-switch-menu" id={id} onKeyDown={onPanelKey}>
      <div className="nav-switch-search">
        <Search className="nav-switch-search-icon" strokeWidth={1.5} aria-hidden="true" />
        <input
          ref={input}
          type="text"
          className="nav-switch-input"
          role="combobox"
          aria-label="Find a project"
          aria-expanded="true"
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={active < 0 ? undefined : optionId(active)}
          placeholder="Find a project…"
          autoComplete="off"
          spellCheck={false}
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setHot(0);
          }}
          onKeyDown={onInputKey}
        />
        <kbd className="nav-switch-kbd" aria-hidden="true">
          {SWITCH_HINT}
        </kbd>
      </div>

      <div className="nav-switch-scroll">
        <div role="listbox" id={listId} aria-label="Switch to">
          {nucleo === undefined ? null : option(nucleo, 0)}
          {/*
            Nothing about projects until the daemon has answered: no count and no
            "no projects" line, because `undefined` is "it has not spoken", and a
            sentence about what it did not say is a claim nobody measured.
          */}
          {projects === undefined ? null : (
            <div role="group" aria-labelledby={labelId}>
              <div className="nav-switch-section" id={labelId}>
                <span>Projects</span>
                <span className="nav-switch-total">{matches.length}</span>
              </div>
              {options.slice(offset).map((entry, index) => option(entry, index + offset))}
            </div>
          )}
        </div>
        {projects !== undefined && typed !== "" && matches.length === 0 ? (
          <p className="nav-switch-empty">No project called “{query.trim()}”.</p>
        ) : null}
      </div>

      <div className="nav-switch-foot">
        <Link to="/projects" className="nav-switch-item" activeOptions={{ exact: true }} onClick={() => onClose(false)}>
          <LayoutList className="nav-switch-foot-icon" strokeWidth={1.5} aria-hidden="true" />
          <span className="nav-switch-label">All projects</span>
        </Link>
        <Link to="/projects/new" className="nav-switch-item" onClick={() => onClose(false)}>
          <Plus className="nav-switch-foot-icon" strokeWidth={1.5} aria-hidden="true" />
          <span className="nav-switch-label">New project</span>
        </Link>
      </div>
    </div>
  );
}

/**
 * The project switcher, in the slot the wordmark used to hold alone — and since
 * 2026-10-02 the one list of projects in the app's chrome.
 *
 * The rail used to carry the roster as rows of its own, shown or hidden by a
 * disclosure. That put a list whose length belongs to the daemon inside a list
 * of destinations whose length is ours: fifteen projects pushed Work and Pillars
 * off the bottom. The rail now has one `Projects` row, and this is where a
 * project is chosen — from anywhere, by pointer or by Ctrl+P and a few letters.
 */
function ProjectSwitcher({
  projects,
  pathname,
  collapsed,
  open,
  onOpenChange,
  onExpand,
}: {
  projects: ProjectNavEntry[] | undefined;
  pathname: string;
  collapsed: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /**
   * What the button does at icon width, where it is not a switcher at all.
   *
   * A rail collapsed to 56px has no room for the name, the note or the chevron,
   * and a panel opening out of a 56px strip has no anchor. So the button becomes
   * the way back out — which is also the only control the collapsed rail then
   * needs. Ctrl+P still works there: it expands the rail first.
   */
  onExpand: () => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const panelId = `${useId()}-switch`;
  const showing = open && !collapsed;

  const here = projectOf(pathname);
  const hereMode = projects?.find((project) => project.id === here)?.mode;
  const name = here ?? "NucleOS";
  // The mode in words where the menu only draws a dot — the button is the workspace's own line.
  const note = here === undefined ? "the núcleo" : hereMode === undefined ? "project" : modeWord(hereMode);

  /*
    Closing on a press anywhere else, without a full-screen invisible overlay.
    That overlay swallows the first click on whatever you were actually reaching
    for, and it is invisible to the keyboard.
  */
  useEffect(() => {
    if (!showing) return;

    function onPointerDown(event: PointerEvent) {
      if (root.current?.contains(event.target as Node) === true) return;
      onOpenChange(false);
    }

    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [showing, onOpenChange]);

  function close(refocus: boolean) {
    onOpenChange(false);
    if (refocus) button.current?.focus();
  }

  return (
    <div className="nav-switch" ref={root}>
      <button
        ref={button}
        type="button"
        className="nav-switch-button"
        /*
          Two controls in one box, and the attributes say which one it currently
          is. Collapsed it is not a disclosure at all, so it carries no
          `aria-expanded` and no `aria-controls` — claiming to own a panel that
          cannot open is worse than claiming nothing.
        */
        aria-expanded={collapsed ? undefined : showing}
        aria-controls={collapsed || !showing ? undefined : panelId}
        /*
          Spelled out rather than left to the button's text, which would run the
          name into the note ("NucleOS the núcleo"). Collapsed there is no text at
          all, and a button whose whole content is an `alt=""` image has no name.
        */
        aria-label={collapsed ? "Expand the sidebar" : `${name} — switch project`}
        title={collapsed ? "Expand the sidebar" : undefined}
        onClick={collapsed ? onExpand : () => onOpenChange(!showing)}
      >
        {/*
          The lead box: the mark on the núcleo, the project's initial inside one.
          The mark is the one place in the rail the brand appears, and `--accent`
          is never spent on state — so the mode inside a project is a dot in the
          corner, on its own tone, and never a cyan anything.
        */}
        <span className="nav-switch-lead">
          {here === undefined ? (
            <img className="nav-switch-mark" src={mark} alt="" />
          ) : (
            <>
              <span className="nav-switch-initial">{here.charAt(0).toUpperCase()}</span>
              {hereMode === undefined ? null : (
                <span className="nav-switch-dot nav-switch-corner" data-mode={hereMode} />
              )}
            </>
          )}
        </span>
        <span className="nav-switch-text">
          <span className="nav-switch-name">{name}</span>
          <span className="nav-switch-note">{note}</span>
        </span>
        <ChevronsUpDown className="nav-switch-chevron" strokeWidth={1.5} aria-hidden="true" />
      </button>

      {showing ? <SwitcherPanel id={panelId} projects={projects} here={here} onClose={close} /> : null}
    </div>
  );
}

/* ------------------------------------------------------------------ rail -- */

export interface SidebarProps {
  /**
   * Counts for the items that carry one, by badge source.
   *
   * A count the shell has no source for yet is simply absent, and an absent
   * count renders no badge — which is honest. A zero renders no badge either,
   * for a different reason: a badge is a summons, and "0 waiting" summons
   * nobody while still taking the eye.
   */
  badges?: Partial<Record<NavBadge, number>>;
  /**
   * The project roster, for the switcher — the one list of projects.
   *
   * Passed in rather than fetched here, which is the same rule the badges follow
   * and for the same reason: this component needs a router and nothing else, and
   * that is what lets the entire rail be tested without a daemon.
   *
   * `undefined` — the roster has not answered — offers the núcleo and the
   * footer, and says nothing about how many projects there are: the daemon has
   * not spoken, and a sentence about what it did not say is a claim nobody
   * measured.
   */
  projects?: ProjectNavEntry[];
  /**
   * Something in the machine wants looking at; System gets a dot.
   *
   * A boolean and not the readout itself, because the rail draws a dot and has
   * no business deciding what counts as trouble — `AppShell` reads the daemon's
   * subsystem aggregate and answers that question there, where the query lives.
   */
  systemAlert?: boolean;
  /**
   * The control that shares System's row — the notifications drawer, today.
   *
   * A slot for the same reason `children` is one: the drawer needs live queries
   * and this component needs a router and nothing else. Both halves of the row
   * are drawn as marks only, with the word in the accessible name and the title.
   */
  besideSystem?: ReactNode;
  /**
   * The pinned footer — connection line, budget, kill switch.
   *
   * Passed in rather than built here so that this component stays a *navigation*
   * component: it needs a router and nothing else, while the footer needs three
   * live queries. That split is what lets the whole rail be tested without a
   * daemon, and it keeps the pinning — the part that must never break — in one
   * layout rule rather than spread across three components.
   */
  children?: ReactNode;
}

/**
 * The left rail: every page in the app, and the state of the machine under it.
 *
 * The switcher, then three groups, then a pinned footer. The pinning is the
 * load-bearing part: the kill switch has to be reachable without scrolling, from
 * every page, at any scroll position of the item list — so the scrolling region
 * is the groups alone and the footer is its sibling, not its last child.
 *
 * The drawing is the `dashboard-sidebar` paste, adopted 2026-09-05 after a
 * side-by-side lab: lucide icons at 16px, 13px rows, the switcher above the
 * scroll. Three of its decisions were NOT adopted, each for a measured reason:
 *
 *  - **The hidden scrollbar.** The paste sets `[&::-webkit-scrollbar]:hidden`
 *    and hides the overflow indicator on all three engines. With this app's
 *    26-row nav that left 314px and eight destinations unreachable with no sign
 *    they existed. The rail keeps its scrollbar and its fade at the cut.
 *  - **The neutral active fill.** The paste marks the current row with
 *    `bg-black/5` and hovers with `bg-black/5` — the same value, so in light
 *    mode the page you are on and the row under the pointer are indistinguishable.
 *    The accent bar stays.
 *  - **The shortcut chips and the Search row.** Both are real in the paste and
 *    neither is wired here: this app's palette is per-page, not rail-level.
 *    Drawing them would be an invented affordance, which is the one failure mode
 *    an adoption like this makes easy.
 */
export function Sidebar({ badges, projects, systemAlert, besideSystem, children }: SidebarProps) {
  const [collapsed, setCollapsed] = useState(readCollapsed);
  /**
   * What is *drawn* at icon width, as opposed to what the rail is currently wide.
   *
   * The two are the same except for the 240ms the rail spends closing, and that
   * gap is the animation. `collapsed` is the answer to "which width", and it
   * changes on the click; `iconsOnly` is the answer to "which layout", and on the
   * way in it waits for the slide to finish. So the words are covered by a rail
   * getting narrower — each row clips its own label — instead of being switched
   * off in front of somebody while the rail is still wide around them.
   *
   * Opening does not wait: the layout comes back on the first frame and the
   * widening uncovers it, which is the same effect run backwards.
   *
   * Both start from storage, so a rail that opens already shut has no lag to
   * play: a transition needs a change, and there is none on the first paint.
   */
  const [iconsOnly, setIconsOnly] = useState(readCollapsed);
  const [switcherOpen, setSwitcherOpen] = useState(false);
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const navigate = useNavigate();

  function toggleCollapsed() {
    const next = !collapsed;
    setCollapsed(next);
    writeCollapsed(next);
    if (next) setSwitcherOpen(false);
    else setIconsOnly(false);
  }

  /**
   * Ctrl+P (Cmd+P) opens the switcher from anywhere in the window.
   *
   * `preventDefault` is not optional: unhandled, the chord is Print. A collapsed
   * rail is expanded first — the panel hangs off the open rail's button — and
   * the icons-only layout comes off on the same render, so the panel can mount
   * at once. `defaultPrevented` lets anything that already answered the chord
   * keep it, which is the same courtesy the palette's Ctrl+K extends.
   */
  useEffect(() => {
    function onKey(event: globalThis.KeyboardEvent) {
      if (event.defaultPrevented) return;
      if (!(event.ctrlKey || event.metaKey) || event.altKey || event.shiftKey) return;
      if (event.key.toLowerCase() !== "p" || typingElsewhere(event.target)) return;
      event.preventDefault();
      if (collapsed) {
        setCollapsed(false);
        writeCollapsed(false);
        setIconsOnly(false);
      }
      setSwitcherOpen(true);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [collapsed]);

  /**
   * The end of the slide, which is when the icons-only layout is allowed in.
   *
   * Read from the event rather than timed against the CSS, so the two cannot
   * drift apart — and `reduce`d motion, where `base.css` cuts the duration to
   * 0.01ms, arrives here on the very next frame rather than not at all.
   *
   * Both guards earn their place: `currentTarget` because a transition inside the
   * rail — a row taking its hover colour — bubbles through here, and `width`
   * because it is the one property whose ending means the rail has arrived.
   */
  function onSlideEnd(event: TransitionEvent<HTMLElement>) {
    if (event.target !== event.currentTarget || event.propertyName !== "width") return;
    setIconsOnly(collapsed);
  }

  /**
   * The net under `transitionend`, for the places where transitions do not run.
   *
   * A transition that never starts never ends, and then the rail would sit at
   * 56px wearing its open layout for ever — labels clipped to nothing, no way to
   * tell it is shut. That is a worse failure than a missing animation, and it is
   * not hypothetical: it is exactly what jsdom does, and what a browser does for
   * a duration of a flat zero.
   *
   * Longer than the slide on purpose. This is a fallback, not the mechanism —
   * when the event arrives it has already set the state, this effect sees the two
   * agree, and the timer is cleared without firing.
   */
  useEffect(() => {
    if (collapsed === iconsOnly) return;
    const timer = setTimeout(() => setIconsOnly(collapsed), 400);
    return () => clearTimeout(timer);
  }, [collapsed, iconsOnly]);

  /**
   * Keyboard handling for the whole rail, in one place.
   *
   * Arrow keys walk the items and wrap; Home and End jump to the ends. Enter
   * navigates — and takes the navigation itself rather than letting the anchor's
   * default do it, because the two would otherwise both fire and the router
   * would see the same destination twice.
   *
   * Scoped to `[data-nav-path]`, which the switcher's links deliberately do not
   * carry: a menu that is open has its own up-and-down, and rows that walked
   * into the rail behind it would leave focus somewhere the reader cannot see.
   */
  function handleKeyDown(event: KeyboardEvent<HTMLElement>) {
    const focused = (event.target as HTMLElement).closest<HTMLElement>("[data-nav-path]");

    if (event.key === "Enter") {
      const path = focused?.dataset.navPath;
      if (path === undefined) return;
      event.preventDefault();
      void navigate({ to: path });
      return;
    }

    if (event.key !== "ArrowDown" && event.key !== "ArrowUp" && event.key !== "Home" && event.key !== "End") {
      return;
    }

    const items = Array.from(event.currentTarget.querySelectorAll<HTMLElement>("[data-nav-path]"));
    if (items.length === 0 || focused === null) return;
    const current = items.indexOf(focused);
    if (current < 0) return;

    let next: number;
    if (event.key === "Home") next = 0;
    else if (event.key === "End") next = items.length - 1;
    else if (event.key === "ArrowDown") next = (current + 1) % items.length;
    else next = (current - 1 + items.length) % items.length;

    event.preventDefault();
    items[next].focus();
  }

  /** One row. */
  function item(entry: NavItem, options: { alert?: boolean; markOnly?: boolean } = {}) {
    const { alert = false, markOnly = false } = options;
    const active = isActive(pathname, entry.path);
    const count = entry.badge === undefined ? undefined : badges?.[entry.badge];
    const noun = entry.badge === undefined ? "" : BADGE_NOUN[entry.badge];
    const counted = count !== undefined && count > 0;
    const classes = ["nav-item"];
    if (active) classes.push("nav-item-active");
    // Dimmed, never removed. A pillar nobody configured and a feature the núcleo
    // cannot serve yet are both still part of the design, and hiding them would
    // be the shell pretending they were never drawn.
    if (entry.disabled !== undefined) classes.push("nav-item-disabled");

    const Icon = entry.icon;

    return (
      <Link
        to={entry.path}
        data-nav-path={entry.path}
        className={classes.join(" ")}
        aria-current={active ? "page" : undefined}
        /*
          Spelled out rather than left to the name computation over the
          children, which concatenates adjacent inline text with no separator
          and would announce this item as "Waiting7". A count that is on screen
          and not in the accessible name is a summons only some people get.
        */
        aria-label={spokenName(entry.label, counted ? count : undefined, noun, alert)}
        title={entry.disabled ?? (iconsOnly || markOnly ? entry.label : undefined)}
      >
        <Icon className="nav-icon" strokeWidth={1.5} aria-hidden="true" />
        <span className="nav-label">{entry.label}</span>
        {counted ? (
          <span className="nav-badge" aria-hidden="true">
            {count}
          </span>
        ) : null}
        {alert ? <span className="nav-dot" aria-hidden="true" /> : null}
      </Link>
    );
  }

  return (
    <nav
      /*
        Two classes and not one: `nav-narrow` is the width, `nav-collapsed` is
        the icons-only drawing, and they disagree for the length of the slide.
        See `iconsOnly` above.
      */
      className={["nav", collapsed ? "nav-narrow" : null, iconsOnly ? "nav-collapsed" : null]
        .filter((name) => name !== null)
        .join(" ")}
      aria-label="Sections"
      onKeyDown={handleKeyDown}
      onTransitionEnd={onSlideEnd}
    >
      <div className="nav-brand">
        <ProjectSwitcher
          projects={projects}
          pathname={pathname}
          collapsed={iconsOnly}
          open={switcherOpen}
          onOpenChange={setSwitcherOpen}
          onExpand={toggleCollapsed}
        />
        {/*
          Only on the way in, and that is the whole of the change: collapsed, the
          mark above is the way back out, so a second arrow beside it would be a
          duplicate control in the one place with no room for it — and the row
          keeps exactly the height it had, which is what stops the rail's rows
          from shifting under the pointer when the state changes.

          A raw button rather than the `Button` primitive: this is chrome for the
          rail itself, sized to the rail, and `Button` deliberately refuses a
          className so that nothing can quietly grow a fourteenth variant.

          No `aria-pressed`. This is no longer one toggle in two states but two
          controls, each of which does one thing, and a pressed state on a button
          that disappears when pressed says nothing anybody can use.
        */}
        {iconsOnly ? null : (
          <button
            type="button"
            className="nav-collapse"
            onClick={toggleCollapsed}
            aria-label="Collapse the sidebar to icons"
            title="Collapse the sidebar to icons"
          >
            <PanelLeftClose className="nav-collapse-icon" strokeWidth={1.5} aria-hidden="true" />
          </button>
        )}
      </div>

      <div className="nav-scroll">
        {NAV.map((group) => (
          <div className="nav-group" key={group.id}>
            <h2 className="nav-group-label" id={`nav-group-${group.id}`}>
              {group.label}
            </h2>
            <ul className="nav-list" aria-labelledby={`nav-group-${group.id}`}>
              {group.items.map((entry) => (
                <li key={entry.id}>{item(entry)}</li>
              ))}
            </ul>
          </div>
        ))}
      </div>

      <div className="nav-footer">
        {/*
          System and the drawer split one row, marks only (owner's call,
          2026-10-05): two places you go when told to, not two rows of the rail.
        */}
        <div className="nav-footer-pair">
          {item(SYSTEM_ITEM, { alert: systemAlert === true, markOnly: true })}
          {besideSystem}
        </div>
        {/*
          The rule moved out of here on 2026-09-05 and into the slot below.
          It used to sit between System and everything else, which read as "the
          first row, then the rest" — but the break that matters in this footer
          is between the things you can go to or read and the one control that
          stops the machine. Only whoever fills the slot knows where that break
          falls, so the slot draws it.
        */}
        {children}
      </div>
    </nav>
  );
}

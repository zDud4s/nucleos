import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode, type TransitionEvent } from "react";
import { Link, useNavigate, useRouterState } from "@tanstack/react-router";
import { ChevronDown, ChevronRight, PanelLeftClose, Plus, type LucideIcon } from "lucide-react";
import { NAV, SYSTEM_ITEM, type NavBadge, type NavItem } from "./nav";
import type { AutopilotMode } from "../data/system";
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
 * Whether the project list under `All projects` is open.
 *
 * Same storage and same reasoning as the collapse above: one window, one screen.
 */
const ROSTER_KEY = "nucleos.sidebar.projects";

/**
 * Three answers, not two.
 *
 * `undefined` — nothing stored, nobody has said — means **the route decides**,
 * which is exactly the rule the rail followed before the disclosure existed: the
 * list is open where projects are the subject and closed everywhere else. The
 * moment somebody opens or closes it by hand that answer is theirs, and it is
 * kept, on every page, until they change it again. A dropdown that silently
 * re-decided itself on navigation would not be a control.
 */
function readRosterOpen(): boolean | undefined {
  try {
    const stored = window.localStorage.getItem(ROSTER_KEY);
    return stored === null ? undefined : stored === "1";
  } catch {
    return undefined;
  }
}

function writeRosterOpen(open: boolean): void {
  try {
    window.localStorage.setItem(ROSTER_KEY, open ? "1" : "0");
  } catch {
    // See `writeCollapsed`.
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
function spokenName(label: string, count: number | undefined, alert: boolean): string | undefined {
  const extras: string[] = [];
  if (count !== undefined) extras.push(`${count} waiting`);
  if (alert) extras.push("needs attention");
  return extras.length === 0 ? undefined : `${label}, ${extras.join(", ")}`;
}

/**
 * One project, as the rail needs it.
 *
 * Deliberately not `ProjectSummary`: the rail wants three fields and that type
 * has fifteen, and taking the whole thing would tie the sidebar to the roster
 * route's shape. What is here is what is drawn — the name, the dot, the summons.
 */
export interface ProjectNavEntry {
  /** The daemon's project id, which is also the name it is known by. */
  id: string;
  mode: AutopilotMode;
  /** Decisions waiting in this project. Zero draws no badge — a badge is a summons. */
  pending: number;
}

/**
 * Where in the path a project is, so a workspace stays lit across its modes.
 *
 * The rail's ordinary prefix rule compares against the item's own path, which
 * for a project is its Estado mode. Someone reading the Workflows mode is still
 * *in* that project, and a rail that went dark on the way between modes would
 * say they had left it.
 */
function projectOf(pathname: string): string | undefined {
  const parts = pathname.split("/").filter((part) => part !== "");
  return parts[0] === "projects" ? parts[1] : undefined;
}

/**
 * Whether the roster belongs on screen **unless somebody says otherwise**.
 *
 * This used to be the whole rule. Since the disclosure on `All projects` it is
 * the default the disclosure starts from — see `readRosterOpen` — and the
 * argument below is why that default is this and not "always" or "never".
 *
 * **The rail is destinations; the roster is content, and content grows.** Every
 * other entry in the sidebar is one of a fixed list decided at design time — one
 * more project is one more row, for ever, and a machine watching fifteen of them
 * pushes Work and Pillars off the bottom edge to show names that are only useful
 * to somebody already working in one of them. The rail was the wrong home for a
 * list whose length is not ours to choose.
 *
 * So the rows appear where they are the subject: on `/projects`, and inside a
 * workspace. That keeps the one thing they were promoted for — switching
 * projects without going back out to the list, which is what a day of work
 * actually consists of — and gives back the space everywhere else, where a
 * project name is a destination you reach through `All projects` like any other
 * page.
 *
 * `/projects/new` counts, deliberately: the wizard is in the area, and a rail
 * that emptied while somebody added a project would read as having lost them.
 *
 * Since 2026-09-05 the switcher above the scroll answers the same question from
 * anywhere, which is what lets this stay narrow rather than widening to "always".
 */
function inProjects(pathname: string): boolean {
  return pathname.split("/").filter((part) => part !== "")[0] === "projects";
}

/* --------------------------------------------------------------- switcher -- */

/** The path a project opens at — Estado, the mode that answers the arrival question. */
function projectPath(id: string): string {
  return `/projects/${id}/state`;
}

/**
 * One destination in the switcher: the núcleo, or a project.
 *
 * `note` is the small line under the name and is drawn **only for the selected
 * row**, never in the list. That split is this app's existing rule rather than
 * a new one — see `.nav-mode` in `app.css`: "A colour rather than a word because
 * it sits in a rail read at a glance; the word is on the workspace itself, where
 * it is read on purpose." The menu is the glance; the button is the workspace.
 */
interface SwitcherRow {
  label: string;
  note: string;
  path: string;
  /** A project's autopilot mode, as a dot. Absent on the núcleo, which has none. */
  mode?: AutopilotMode;
  /**
   * This row is the núcleo, and draws the mark where a project draws its dot.
   *
   * A separator alone cannot carry that difference: a project named `nucleos`
   * would sit one line under `NucleOS` — same word, same weight, told apart only
   * by which side of a hairline it fell on. The mark against a dot is a
   * difference of KIND, and no project name can collide with it.
   */
  brand?: true;
}

/**
 * The project switcher, in the slot the wordmark used to hold alone.
 *
 * Why a switcher is what belongs there: the Projects group was promoted from a
 * single item precisely because reaching a project cost "a list to open and then
 * a choice, on every single entry" (`nav.ts`), and then the roster rows were made
 * conditional, which gave that cost back everywhere outside the projects area.
 * Pinned above the scroll, the switcher pays it once more without the roster's
 * length pushing Work and Pillars off the bottom.
 *
 * A disclosure and not a `role="menu"`. Menu semantics come with a keyboard
 * contract — typeahead, roving tabindex, focus trapping — that is easy to
 * half-implement and worse half-implemented than not claimed at all. These are
 * links to pages, they are announced as links, and Escape puts focus back where
 * it came from.
 */
function ProjectSwitcher({
  projects,
  pathname,
  collapsed,
  onExpand,
}: {
  projects: ProjectNavEntry[] | undefined;
  pathname: string;
  collapsed: boolean;
  /**
   * What the mark does at icon width, where it is not a switcher at all.
   *
   * A rail collapsed to 56px has no room for the name, the note or the chevron,
   * and a menu opening out of a 56px strip is a panel with no anchor. So the
   * mark stops being the switcher and becomes the way back out — which is also
   * the only control the collapsed rail then needs, and one less arrow beside
   * it. Switching project is a thing you do from the open rail.
   */
  onExpand: () => void;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);

  /*
    The núcleo first and separated, because it is not a project: it is the way
    back out to Home. The projects follow in the roster's own order, which is the
    daemon's, so the switcher and the rail's rows never disagree about it.
  */
  const rows: SwitcherRow[] = [
    { label: "NucleOS", note: "the núcleo", path: "/", brand: true },
    ...(projects ?? []).map((project) => ({
      label: project.id,
      note: project.mode,
      path: projectPath(project.id),
      mode: project.mode,
    })),
  ];

  const here = projectOf(pathname);
  const current = rows.find((row) => row.label === here) ?? rows[0];

  /*
    Closing on a click anywhere else, without the paste's full-screen invisible
    overlay. That overlay swallows the first click on whatever you were actually
    reaching for, and it is invisible to the keyboard, so it solves the pointer
    case by making the pointer worse and leaves Escape unimplemented.
  */
  useEffect(() => {
    if (!open) return;

    function onPointerDown(event: PointerEvent) {
      if (root.current?.contains(event.target as Node) === true) return;
      setOpen(false);
    }

    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open]);

  /**
   * Arrows walk the list, Escape closes it and hands focus back.
   *
   * Handing focus back is the part that is not decoration: a menu that closes
   * and drops focus on `document.body` leaves a keyboard reader at the top of
   * the page, which is a worse place than the one they opened it from.
   */
  function onKeyDown(event: KeyboardEvent<HTMLUListElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      setOpen(false);
      button.current?.focus();
      return;
    }

    if (event.key !== "ArrowDown" && event.key !== "ArrowUp" && event.key !== "Home" && event.key !== "End") {
      return;
    }

    const links = Array.from(event.currentTarget.querySelectorAll<HTMLElement>("a"));
    const index = links.indexOf(event.target as HTMLElement);
    if (links.length === 0 || index < 0) return;

    let next: number;
    if (event.key === "Home") next = 0;
    else if (event.key === "End") next = links.length - 1;
    else if (event.key === "ArrowDown") next = (index + 1) % links.length;
    else next = (index - 1 + links.length) % links.length;

    event.preventDefault();
    links[next].focus();
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
          `aria-expanded` and no `aria-controls` — claiming to own a menu that
          cannot open is worse than claiming nothing.
        */
        aria-expanded={collapsed ? undefined : open}
        aria-controls={collapsed ? undefined : "nav-switch-menu"}
        /*
          The name is spelled out rather than left to the button's text, which
          would run the name into the note and announce the núcleo as "NucleOS
          the núcleo". Collapsed there is no text at all, and a button whose
          whole content is an `alt=""` image has no name to announce.
        */
        aria-label={collapsed ? "Expand the sidebar" : `${current.label} — switch project`}
        title={collapsed ? "Expand the sidebar" : undefined}
        onClick={collapsed ? onExpand : () => setOpen(!open)}
      >
        {/*
          The one place in the rail the brand is allowed to appear. `tokens.css`
          is explicit that `--accent` is "the one brand colour. Not a state;
          never used to mean anything", so cyan here means NucleOS and may not
          also mean "three things are waiting" — which is why the badges below
          stay on the pending tone.
        */}
        <img className="nav-switch-mark" src={mark} alt="" />
        <span className="nav-switch-text">
          <span className="nav-switch-name">{current.label}</span>
          <span className="nav-switch-note">{current.note}</span>
        </span>
        <ChevronDown className="nav-switch-chevron" strokeWidth={1.5} aria-hidden="true" />
      </button>

      {open ? (
        <ul className="nav-switch-menu" id="nav-switch-menu" onKeyDown={onKeyDown}>
          {rows.map((row) => (
            <li key={row.label} className={row.brand === true ? "nav-switch-brand" : undefined}>
              <Link
                to={row.path}
                className="nav-switch-item"
                /*
                  `page`, and not the `true` a switcher would otherwise take:
                  the router marks the link whose path it is on with `page` of
                  its own accord, and two values for one meaning would make the
                  same row announce differently depending on which of its three
                  modes you happened to be reading. The prop still does work —
                  from `/projects/alpha/workflows` the row points at `/state`,
                  which the router does not consider current, and the switcher
                  does.
                */
                aria-current={row === current ? "page" : undefined}
                /*
                  The mode is a dot on screen and a word in the accessible name.
                  Not a contradiction of the rule above but the whole of it: the
                  word costs no space in speech, and a state carried in colour
                  alone is a state some readers never get. `title` gives the
                  pointer the same word, for anyone who can see the dot and
                  cannot tell the three tones apart.
                */
                aria-label={row.mode === undefined ? undefined : `${row.label}, ${row.mode}`}
                title={row.mode}
                onClick={() => setOpen(false)}
              >
                <span className="nav-switch-lead">
                  {row.brand === true ? (
                    <img className="nav-switch-mini" src={mark} alt="" />
                  ) : (
                    <span className="nav-mode" data-mode={row.mode} aria-hidden="true" />
                  )}
                </span>
                <span className="nav-switch-label">{row.label}</span>
              </Link>
            </li>
          ))}
          <li>
            {/*
              `New project` and not the paste's `Create Workspace`: `/projects/new`
              is a route this app has, so the row goes somewhere. The paste's label
              could not.
            */}
            <Link to="/projects/new" className="nav-switch-item nav-switch-new" onClick={() => setOpen(false)}>
              <span className="nav-switch-lead">
                <Plus className="nav-switch-plus" strokeWidth={1.5} aria-hidden="true" />
              </span>
              <span className="nav-switch-label">New project</span>
            </Link>
          </li>
        </ul>
      ) : null}
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
   * The project roster, for the one group whose items are not in the nav table.
   *
   * Passed in rather than fetched here, which is the same rule the badges follow
   * and for the same reason: this component needs a router and nothing else, and
   * that is what lets the entire rail be tested without a daemon.
   *
   * `undefined` — the roster has not answered — draws no rows at all. Not an
   * empty group and not a "no projects" line: the daemon has not spoken, and a
   * sentence about what it did not say is a claim nobody measured. The switcher
   * reads the same list and follows the same rule: with no answer it offers the
   * núcleo and the way to make a first project, and says nothing about how many
   * there are.
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
 * The switcher, then four groups, then a pinned footer. The pinning is the
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
export function Sidebar({ badges, projects, systemAlert, children }: SidebarProps) {
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
  const [rosterOpen, setRosterOpen] = useState(readRosterOpen);
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const navigate = useNavigate();

  function toggleCollapsed() {
    const next = !collapsed;
    setCollapsed(next);
    writeCollapsed(next);
    if (!next) setIconsOnly(false);
  }

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

  /** The route's answer until somebody gives one of their own. */
  const showRoster = rosterOpen ?? inProjects(pathname);

  function toggleRoster() {
    const next = !showRoster;
    setRosterOpen(next);
    writeRosterOpen(next);
  }

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

  /**
   * One row.
   *
   * `monogram` is the roster's exception to the icon rule and the only one.
   * Every compiled row has a mark of its own from `nav.ts`; a project's identity
   * is its name, and twenty projects drawn with the same generic icon would be
   * twenty identical rows in the collapsed rail. Two letters collide sometimes,
   * which is accepted rather than solved — a generated monogram nobody
   * recognises is worse, and the collapsed rail is a shortcut for a rail you
   * already know.
   *
   * So `icon` is optional *here* and required in `nav.ts`: a roster row has
   * nothing to put in the field and must not be made to invent one, while a
   * compiled row still cannot reach the rail without a mark.
   */
  function item(
    entry: Omit<NavItem, "icon"> & { icon?: LucideIcon },
    options: { alert?: boolean; active?: boolean; mode?: AutopilotMode; monogram?: string } = {},
  ) {
    const { alert = false, active: activeOverride, mode, monogram } = options;
    const active = activeOverride ?? isActive(pathname, entry.path);
    const count =
      entry.badge === undefined
        ? projects?.find((project) => `project:${project.id}` === entry.id)?.pending
        : badges?.[entry.badge];
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
        aria-label={spokenName(entry.label, counted ? count : undefined, alert)}
        title={entry.disabled ?? (iconsOnly ? entry.label : undefined)}
      >
        {monogram !== undefined ? (
          <span className="nav-glyph" aria-hidden="true">
            {monogram}
          </span>
        ) : Icon === undefined ? null : (
          <Icon className="nav-icon" strokeWidth={1.5} aria-hidden="true" />
        )}
        {/*
          Off, shadow and active are three different things a project can be
          doing, and the difference governs whether anything happens here without
          being asked. A colour rather than a word because it sits in a rail read
          at a glance; the word is on the workspace itself, where it is read on
          purpose.
        */}
        {mode === undefined ? null : <span className="nav-mode" data-mode={mode} aria-hidden="true" />}
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
              {group.items.map((entry, index) => {
                /*
                  The roster hangs off the LAST item of a roster group, which is
                  the row that opens the same list in full. Written as a position
                  rather than as `entry.id === "projects"` so that the rail keeps
                  no private knowledge of which row that is — `nav.ts` decides the
                  order, and this follows it.
                */
                const discloses = group.roster === true && index === group.items.length - 1;

                if (!discloses) return <li key={entry.id}>{item(entry)}</li>;

                return (
                  <li key={entry.id}>
                    {/*
                      The link and its chevron in a box of their own, and the box
                      is what the chevron is centred in. The `<li>` will not do:
                      it is the row AND the list under it, so a chevron centred
                      on it lands four rows down, on another project's badge —
                      measured, at 70px below the row it belongs to. One element,
                      and no magic number that a taller row would falsify.
                    */}
                    <div className="nav-rowhead">
                      {item(entry)}
                      {/*
                        A link and a disclosure, side by side, and deliberately
                        not one control doing both: `All projects` is a page —
                        the fleet-wide reading no single workspace can give — and
                        a row that opened a list instead of going there would
                        have taken a destination away to add a toggle. The
                        chevron is the toggle; the word is still the way in.

                        Drawn only once the roster has answered. A disclosure
                        that opens onto nothing is an affordance that lies, and
                        `undefined` here means the daemon has not spoken yet
                        rather than that there are no projects.
                      */}
                      {projects === undefined ? null : (
                        <button
                          type="button"
                          className="nav-disclose"
                          onClick={toggleRoster}
                          aria-expanded={showRoster}
                          aria-controls="nav-roster"
                          aria-label={showRoster ? "Hide the project list" : "Show the project list"}
                          title={showRoster ? "Hide the project list" : "Show the project list"}
                        >
                          <ChevronRight
                            className={showRoster ? "nav-disclose-icon nav-disclose-open" : "nav-disclose-icon"}
                            strokeWidth={1.5}
                            aria-hidden="true"
                          />
                        </button>
                      )}
                    </div>
                    {/*
                      The list the chevron names, and it exists whether or not it
                      is open: `aria-controls` has to resolve to something, and an
                      empty `<ul>` adds no height. Closed it holds no rows at all
                      rather than hidden ones — the rail's arrow walk reads the
                      DOM, and `hidden` rows would still be in it, sending focus
                      to places nobody can see.
                    */}
                    <ul className="nav-sub" id="nav-roster">
                      {showRoster
                        ? projects?.map((project) => (
                            <li key={project.id}>
                              {item(
                                {
                                  id: `project:${project.id}`,
                                  label: project.id,
                                  // Estado is where a project opens: it is the
                                  // mode that answers the question somebody
                                  // arrives with.
                                  path: projectPath(project.id),
                                  badge: undefined,
                                },
                                {
                                  active: projectOf(pathname) === project.id,
                                  mode: project.mode,
                                  monogram: project.id.slice(0, 2),
                                },
                              )}
                            </li>
                          ))
                        : null}
                    </ul>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
      </div>

      <div className="nav-footer">
        {item(SYSTEM_ITEM, { alert: systemAlert === true })}
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

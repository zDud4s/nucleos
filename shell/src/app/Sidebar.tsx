import { useState, type KeyboardEvent, type ReactNode } from "react";
import { Link, useNavigate, useRouterState } from "@tanstack/react-router";
import { NAV, SYSTEM_ITEM, type NavBadge, type NavItem } from "./nav";

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
  /** The daemon is anything but ok; System gets a dot. */
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
 * Three fixed groups, then a pinned footer. The pinning is the load-bearing
 * part: the kill switch has to be reachable without scrolling, from every page,
 * at any scroll position of the item list — so the scrolling region is the
 * groups alone and the footer is its sibling, not its last child.
 */
export function Sidebar({ badges, systemAlert, children }: SidebarProps) {
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const navigate = useNavigate();

  function toggleCollapsed() {
    const next = !collapsed;
    setCollapsed(next);
    writeCollapsed(next);
  }

  /**
   * Keyboard handling for the whole rail, in one place.
   *
   * Arrow keys walk the items and wrap; Home and End jump to the ends. Enter
   * navigates — and takes the navigation itself rather than letting the anchor's
   * default do it, because the two would otherwise both fire and the router
   * would see the same destination twice.
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

  function item(entry: NavItem, alert = false) {
    const active = isActive(pathname, entry.path);
    const count = entry.badge === undefined ? undefined : badges?.[entry.badge];
    const counted = count !== undefined && count > 0;
    const classes = ["nav-item"];
    if (active) classes.push("nav-item-active");
    // Dimmed, never removed. A pillar nobody configured and a feature the núcleo
    // cannot serve yet are both still part of the design, and hiding them would
    // be the shell pretending they were never drawn.
    if (entry.disabled !== undefined) classes.push("nav-item-disabled");

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
        title={entry.disabled ?? (collapsed ? entry.label : undefined)}
      >
        <span className="nav-glyph" aria-hidden="true">
          {entry.glyph}
        </span>
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
      className={collapsed ? "nav nav-collapsed" : "nav"}
      aria-label="Sections"
      onKeyDown={handleKeyDown}
    >
      <div className="nav-brand">
        <span className="nav-wordmark">NucleOS</span>
        {/*
          A raw button rather than the `Button` primitive: this is chrome for the
          rail itself, sized to the rail, and `Button` deliberately refuses a
          className so that nothing can quietly grow a fourteenth variant.
        */}
        <button
          type="button"
          className="nav-collapse"
          onClick={toggleCollapsed}
          aria-pressed={collapsed}
          aria-label={collapsed ? "Expand the sidebar" : "Collapse the sidebar to icons"}
          title={collapsed ? "Expand the sidebar" : "Collapse the sidebar to icons"}
        >
          <span aria-hidden="true">{collapsed ? "››" : "‹‹"}</span>
        </button>
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
        {item(SYSTEM_ITEM, systemAlert === true)}
        <hr className="nav-rule" />
        {children}
      </div>
    </nav>
  );
}

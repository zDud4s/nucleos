/**
 * The navigation, as data.
 *
 * No JSX in this file, on purpose. The sidebar is the one surface every page is
 * reached through, and the questions worth asking about it — is Teams still in
 * the list, does Waiting still carry the pending badge, did anything quietly
 * fall out of Pillars — are questions about a *table*, not about a render. A
 * test that reads this module answers them without mounting a router.
 *
 * It is also the single list of routes: `router.tsx` builds one route per entry
 * here, so a page cannot be added to the app without appearing in the sidebar,
 * and cannot appear in the sidebar without being reachable.
 */

/** The three fixed groups of the design's §3.1. Order is the reading order. */
export type NavGroupId = "operate" | "work" | "pillars";

/**
 * Where an item's count comes from.
 *
 * A *name*, not a number: the sidebar does not know how to count unread mail,
 * and the AppShell does not know which item wants the count. This is the seam
 * between them, and it is what lets a badge be declared here in the slice that
 * builds the nav and filled in by the slice that builds the pillar.
 */
export type NavBadge = "proposals" | "chats" | "mail";

export interface NavItem {
  /** Stable id — used for badge lookup, disabled sets, and test selectors. */
  id: string;
  label: string;
  /** The route path. Exactly one route is built per item. */
  path: string;
  /**
   * A two-letter monogram for the icon-collapsed rail.
   *
   * Spelled out rather than derived from the label because four items start
   * with C — Chats, Council, Contacts, Calendar — and a rail that shows the
   * same letter four times is worse than no rail. Letters rather than an icon
   * font: the CSP forbids remote assets, and glyphs read the same at any zoom.
   */
  glyph: string;
  badge?: NavBadge;
  /**
   * Why this item cannot do anything yet, when that is a fact about the
   * *núcleo* rather than about this machine's configuration.
   *
   * An item with this set is still in the list and still navigable — it goes to
   * a page that explains itself. Hiding it would be the shell pretending the
   * feature was never designed, which is a worse lie than admitting it is not
   * wired.
   */
  disabled?: string;
}

export interface NavGroup {
  id: NavGroupId;
  /** Shown in small caps above the group. */
  label: string;
  items: NavItem[];
}

/**
 * The sidebar, top to bottom.
 *
 * Operate is the state of the machine right now, Work is the things you and the
 * agents are doing together, Pillars are the outside world the núcleo reaches
 * into. The split is not cosmetic: it is the order in which someone opening the
 * window at 9am wants to be told things.
 */
export const NAV: NavGroup[] = [
  {
    id: "operate",
    label: "Operate",
    items: [
      { id: "home", label: "Home", path: "/", glyph: "Ho" },
      { id: "fleet", label: "Fleet", path: "/fleet", glyph: "Fl" },
      { id: "autopilot", label: "Autopilot", path: "/autopilot", glyph: "Ap" },
      { id: "waiting", label: "Waiting", path: "/waiting", glyph: "Wt", badge: "proposals" },
      { id: "runs", label: "Runs", path: "/runs", glyph: "Ru" },
      { id: "feed", label: "Feed", path: "/feed", glyph: "Fd" },
      { id: "projects", label: "Projects", path: "/projects", glyph: "Pj" },
    ],
  },
  {
    id: "work",
    label: "Work",
    items: [
      { id: "chats", label: "Chats", path: "/chats", glyph: "Ch", badge: "chats" },
      { id: "errands", label: "Errands", path: "/errands", glyph: "Er" },
      {
        id: "teams",
        label: "Teams",
        path: "/teams",
        glyph: "Tm",
        // Verified against `core/src/http.rs`: the team tables exist and no
        // `/teams`, `/team-runs`, `/team-triggers` or `/team-recruits` route is
        // mounted. The page says so rather than offering controls that would
        // 404.
        disabled: "the núcleo has no team routes yet",
      },
      { id: "agents", label: "Agents", path: "/agents", glyph: "Ag" },
      { id: "council", label: "Council", path: "/council", glyph: "Cn" },
    ],
  },
  {
    id: "pillars",
    label: "Pillars",
    items: [
      { id: "mail", label: "Mail", path: "/mail", glyph: "Ml", badge: "mail" },
      { id: "contacts", label: "Contacts", path: "/contacts", glyph: "Ct" },
      { id: "calendar", label: "Calendar", path: "/calendar", glyph: "Cl" },
      { id: "voice", label: "Voice", path: "/voice", glyph: "Vo" },
      { id: "web", label: "Web", path: "/web", glyph: "We" },
      { id: "browser", label: "Browser", path: "/browser", glyph: "Br" },
      { id: "files", label: "Files", path: "/files", glyph: "Fi" },
    ],
  },
];

/**
 * System sits under the rule, with the connection line and the kill switch.
 *
 * Not a fourth group: it is where you go when something in the footer above it
 * told you to, and grouping it with the pillars would put "the machine is
 * broken" in the same list as "read your mail".
 */
export const SYSTEM_ITEM: NavItem = {
  id: "system",
  label: "System",
  path: "/system",
  glyph: "Sy",
};

/** Every item in the sidebar, groups flattened, System last. */
export const NAV_ITEMS: NavItem[] = [...NAV.flatMap((group) => group.items), SYSTEM_ITEM];

/** Every route the app has. `router.tsx` builds exactly this set. */
export const NAV_PATHS: string[] = NAV_ITEMS.map((item) => item.path);

/** The item a path belongs to, or `undefined` for a path the nav does not own. */
export function navItemForPath(path: string): NavItem | undefined {
  return NAV_ITEMS.find((item) => item.path === path);
}

/**
 * Which slice of the build brings an item's page.
 *
 * The groups double as the delivery slices — Operate, Work and Pillars are the
 * order this app is being built in, not only the order it is read in — so a
 * placeholder page can name the slice that will replace it without a second
 * table to keep in step with this one.
 */
export function sliceOf(item: NavItem): string {
  const group = NAV.find((candidate) => candidate.items.some((member) => member.id === item.id));
  return group === undefined ? SYSTEM_ITEM.label : group.label;
}

// §spec novo-frontend
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

/**
 * The groups of the rail, in reading order.
 *
 * Three of them are the design's §3.1 and are fixed lists. `projects` is the
 * fourth and is a different kind of thing: it declares a *position* and is
 * filled from the daemon's roster, because a project's path is not knowable
 * when this file is compiled.
 */
export type NavGroupId = "operate" | "projects" | "work" | "pillars";

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
  /**
   * This group's items are the project roster, not the list above.
   *
   * A marker rather than a check on the id, so that whoever renders the rail
   * asks *what kind of group is this* instead of knowing one id by heart — and
   * so that `items: []` reads as "filled elsewhere" rather than as an oversight.
   *
   * A roster group's rows are also **conditional**, which no other group's are:
   * they are drawn only while the reader is in the projects area. The rail is a
   * fixed list of destinations and a roster is not one — see `inProjects` in
   * `Sidebar.tsx` for the argument. The group itself is unconditional: its
   * heading and `All projects` are always there, so the position never moves.
   *
   * The consequence worth stating: entries in a roster group are deliberately
   * NOT in {@link NAV_PATHS}. That list is the route list, one route built per
   * entry, and it can only contain paths that exist at compile time. A project
   * is reached through the parameterised route in `router.tsx` instead, so the
   * "no page outside the sidebar" invariant still holds — projects are in the
   * sidebar, just not by this mechanism.
   */
  roster?: true;
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
      /**
       * Under Operate and not under Work, although it is the closest thing the
       * app has to a document: what the agent has been told is a fact about the
       * machine's current behaviour, not a thing you and it are doing together.
       * It sits after Projects because a lesson is scoped to one.
       */
      { id: "learned", label: "Learned", path: "/learned", glyph: "Ln" },
    ],
  },
  /**
   * Projects by name, between the machine's state and the work being done in it.
   *
   * A group and not an item, and it is the item promoted rather than a new
   * neighbour for it: with one item, reaching a project costs opening a list and
   * then choosing from it, on every single entry, and the workspace is where a
   * day is spent. Leaving both would have put two things called Projects in one
   * rail, which is the collision that settles the question.
   *
   * `All projects` is the old item, kept as this group's first entry: the roster
   * answers "how are all of them doing" and holds the WIP ceiling, which is a
   * fleet-wide reading and not a thing any single workspace can say. The roster
   * rows follow it — one per project, from the daemon — **while you are in the
   * projects area, and not otherwise.**
   *
   * That last clause is a correction to §3.1 of the design, made 2026-08-24 with
   * the group built and in use. The promotion was argued from the cost of
   * reaching a project — a list to open and then a choice, on every entry — and
   * that cost is real *while you are working in one*. What the argument missed is
   * that the rail is otherwise a fixed list of destinations, and this group's
   * length belongs to the daemon: fifteen projects push Work and Pillars off the
   * bottom to show names nobody on the Feed page is looking for. Conditional rows
   * keep the saving where it was earned and give back the space where it was not.
   */
  {
    id: "projects",
    label: "Projects",
    items: [{ id: "projects", label: "All projects", path: "/projects", glyph: "Pj" }],
    roster: true,
  },
  {
    id: "work",
    label: "Work",
    items: [
      { id: "chats", label: "Chats", path: "/chats", glyph: "Ch", badge: "chats" },
      { id: "errands", label: "Errands", path: "/errands", glyph: "Er" },
      { id: "teams", label: "Teams", path: "/teams", glyph: "Tm" },
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

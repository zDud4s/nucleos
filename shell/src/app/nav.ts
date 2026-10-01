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
 *
 * The one import is `lucide-react`, for the icon each row draws. A component
 * reference is not a render — nothing here returns JSX, and the table tests
 * still read this module without mounting anything — and putting the icon
 * beside the label is what makes it impossible to add a page that arrives in
 * the rail with no mark on it.
 */
import {
  Activity,
  Bot,
  Boxes,
  Calendar,
  Compass,
  Contact,
  Files,
  FolderKanban,
  Gauge,
  Globe,
  GraduationCap,
  Hourglass,
  LayoutDashboard,
  Mail,
  MessagesSquare,
  Mic,
  Rss,
  Scale,
  SlidersHorizontal,
  Users,
  type LucideIcon,
} from "lucide-react";

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
export type NavBadge = "proposals" | "chats";

export interface NavItem {
  /** Stable id — used for badge lookup, disabled sets, and test selectors. */
  id: string;
  label: string;
  /** The route path. Exactly one route is built per item. */
  path: string;
  /**
   * The mark beside the label, and the whole of the row when the rail is
   * collapsed to icons.
   *
   * This replaced a two-letter monogram on 2026-09-05. The monogram was chosen
   * against an *icon font*, and the argument was the CSP: remote assets are
   * forbidden, so a font that arrives over the network cannot be used. Lucide
   * is not that — it compiles to inline SVG in the bundle, fetches nothing, and
   * scales at any zoom for the same reason letters did. The other half of the
   * argument — four items start with C, so derived monograms collide — is not
   * an argument for letters, it is an argument against *deriving*, and this
   * field is spelled out one row at a time exactly as `glyph` was.
   *
   * Required, and typed as the component rather than as a name: a row that
   * reaches the rail with nothing to draw is a compile error here rather than a
   * blank column on screen.
   */
  icon: LucideIcon;
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
      { id: "home", label: "Home", path: "/", icon: LayoutDashboard },
      { id: "fleet", label: "Fleet", path: "/fleet", icon: Boxes },
      { id: "autopilot", label: "Autopilot", path: "/autopilot", icon: Gauge },
      { id: "waiting", label: "Waiting", path: "/waiting", icon: Hourglass, badge: "proposals" },
      { id: "runs", label: "Runs", path: "/runs", icon: Activity },
      { id: "feed", label: "Feed", path: "/feed", icon: Rss },
      /**
       * Under Operate and not under Work, although it is the closest thing the
       * app has to a document: what the agent has been told is a fact about the
       * machine's current behaviour, not a thing you and it are doing together.
       * It sits after Projects because a lesson is scoped to one.
       */
      { id: "learned", label: "Learned", path: "/learned", icon: GraduationCap },
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
    items: [{ id: "projects", label: "All projects", path: "/projects", icon: FolderKanban }],
    roster: true,
  },
  {
    id: "work",
    label: "Work",
    items: [
      { id: "chats", label: "Chats", path: "/chats", icon: MessagesSquare, badge: "chats" },
      { id: "teams", label: "Teams", path: "/teams", icon: Users },
      { id: "agents", label: "Agents", path: "/agents", icon: Bot },
      { id: "council", label: "Council", path: "/council", icon: Scale },
    ],
  },
  {
    id: "pillars",
    label: "Pillars",
    items: [
      /*
        No badge, decided 2026-09-22. It counted untriaged mail in the Awaiting You tone, and
        untriaged is the daemon's backlog rather than anything asked of the reader — amber that
        means "the machine has not got there yet" teaches a person to ignore amber everywhere
        else. What genuinely asks for the reader arrives where it can be cleared: an urgent
        message raises `email_urgent` on the feed and in Notifications, and triage falling behind
        raises `email_triage_stalled`. A queue row has no "answered" state, so any count taken
        from it would stand until retention pruned the message — a summons nobody can satisfy.
      */
      { id: "mail", label: "Mail", path: "/mail", icon: Mail },
      { id: "contacts", label: "Contacts", path: "/contacts", icon: Contact },
      { id: "calendar", label: "Calendar", path: "/calendar", icon: Calendar },
      { id: "voice", label: "Voice", path: "/voice", icon: Mic },
      { id: "web", label: "Web", path: "/web", icon: Globe },
      { id: "browser", label: "Browser", path: "/browser", icon: Compass },
      { id: "files", label: "Files", path: "/files", icon: Files },
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
  icon: SlidersHorizontal,
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

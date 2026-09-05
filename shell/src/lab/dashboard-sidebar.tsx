/**
 * `dashboard-sidebar.tsx`, as pasted from the registry.
 *
 * Kept in `src/lab/` and NOT in `src/ui/vendor/` on purpose: this is the copy
 * being evaluated, not a copy the app depends on. Nothing under `src/` imports
 * this file, so it cannot reach the shipped bundle — the same rule
 * `src/preview/` keeps, and for the same reason. When it is adopted it moves to
 * `src/ui/vendor/dashboard-sidebar.tsx`, which is what `components.json`'s
 * `"ui": "@/ui/vendor"` alias names, and this directory goes away.
 *
 * Three changes from the paste, and only three:
 *
 *  1. `groups` / `bottomItems` are props, defaulting to the registry's mock
 *     tables. Verbatim, the nav was a module-level constant, which makes the
 *     question "does this work with OUR tabs" unanswerable without editing the
 *     vendor file — and editing a vendor file is how the next version of it
 *     stops being droppable in. The defaults mean the paste still renders
 *     exactly as shipped when nothing is passed.
 *  2. The `demo.tsx` half of the paste is not here. It was byte-identical to
 *     this file; the harness that replaces it is `main.tsx` next door.
 *  3. `logo` is an optional slot in the workspace switcher. The paste draws a
 *     `bg-primary` square holding the workspace's first letter, which is the
 *     right default for a product that does not know its tenants — and the
 *     wrong one for the single place in this app where the brand belongs.
 *     Absent, the monogram is drawn exactly as it was, so `Acme Corp` still
 *     looks like the registry's screenshot.
 *
 * Everything else — every class, every pixel value, the `useState` per row that
 * makes each item own its own open/closed — is as it arrived. That is the
 * point: the left panel of the lab is a picture of THIS, unretouched.
 */
import React, { useState } from "react";
import {
  Search,
  LayoutDashboard,
  FolderKanban,
  Users,
  Settings,
  LogOut,
  Hash,
  ChevronDown,
  ChevronRight,
  Inbox,
  Calendar,
  Activity,
  CreditCard,
  Globe,
  Terminal,
  Blocks,
  PanelLeftClose,
  PanelLeftOpen,
  Command,
  X,
} from "lucide-react";

export type NavItemData = {
  id: string;
  title: string;
  icon: React.ElementType;
  badge?: number | string;
  shortcut?: string;
  children?: NavItemData[];
};

export type NavGroupData = {
  heading?: string;
  items: NavItemData[];
};

/**
 * One row of the switcher under the mark.
 *
 * The paste took `string[]`, which is right for the tenant list it was drawn
 * for and cannot say the two things this app needs: what each destination IS
 * (a project's autopilot mode) and where the list breaks (the núcleo is not a
 * project and must not sit in the same run as them). Strings are still
 * accepted, so the registry demo passes exactly what it always passed.
 */
export type WorkspaceOption = {
  label: string;
  /**
   * The small line under the name — drawn ONLY when this row is the selected
   * one, never in the list.
   *
   * That split is this app's existing rule, not a new one. `app.css` says of
   * the rail's mode dot: "A colour rather than a word because it sits in a rail
   * read at a glance; the word is on the workspace itself, where it is read on
   * purpose." The menu is the glance, the button is the workspace.
   */
  note?: string;
  /**
   * What this project is doing without being asked, as a dot in the menu.
   *
   * The three tones are already named after the three modes, so this is a
   * substitution rather than a table — exactly as `.nav-mode` does it, and a
   * fourth mode would fail visibly here (no colour) instead of silently picking
   * a wrong one.
   */
  mode?: "active" | "shadow" | "off";
  /**
   * Drawn in the dot's place, for a row that is not a project.
   *
   * The núcleo is not one, and a separator alone cannot say so: a project named
   * `nucleos` would sit one line below `NucleOS` and read as the same thing.
   * A mark against a dot is a difference of KIND, which no name can collide
   * with.
   */
  leading?: React.ReactNode;
  /** Draw the existing `h-px` rule after this row. */
  separatorAfter?: boolean;
};

/**
 * The mode dot, in the rail's own vocabulary.
 *
 * `bg-tone-*-fg` are real utilities here: `tailwind.css` maps the seven state
 * tones into the colour namespace, so this is the same value `.nav-mode` reads
 * and not a second copy of it. `off` is hollow — an inset ring with no fill —
 * for the reason `app.css` gives: the ring says the project is known, the
 * absent fill says nothing is running.
 */
function ModeDot({ mode }: { mode: NonNullable<WorkspaceOption["mode"]> }) {
  const tone =
    mode === "active"
      ? "bg-tone-active-fg"
      : mode === "shadow"
        ? "bg-tone-shadow-fg"
        : "bg-transparent shadow-[inset_0_0_0_1px_var(--tone-off-fg)]";

  return <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${tone}`} aria-hidden="true" />;
}

const asOption = (option: string | WorkspaceOption): WorkspaceOption =>
  typeof option === "string" ? { label: option } : option;

export const mockNavGroups: NavGroupData[] = [
  {
    items: [
      { id: "search", title: "Search", icon: Search, shortcut: "⌘K" },
      { id: "home", title: "Home", icon: LayoutDashboard },
      { id: "inbox", title: "Inbox", icon: Inbox, badge: 12 },
      { id: "analytics", title: "Analytics", icon: Activity },
    ],
  },
  {
    heading: "Workspace",
    items: [
      {
        id: "projects",
        title: "Projects",
        icon: FolderKanban,
        children: [
          { id: "p-active", title: "Active", icon: Hash },
          { id: "p-archived", title: "Archived", icon: Hash },
        ],
      },
      { id: "calendar", title: "Calendar", icon: Calendar },
      {
        id: "team",
        title: "Team",
        icon: Users,
        children: [
          { id: "t-design", title: "Designers", icon: Hash },
          { id: "t-eng", title: "Engineering", icon: Hash },
          { id: "t-product", title: "Product", icon: Hash },
        ],
      },
      {
        id: "customers",
        title: "Customers",
        icon: Globe,
        children: [
          { id: "c-enterprise", title: "Enterprise", icon: Hash },
          { id: "c-smb", title: "SMB", icon: Hash },
        ],
      },
      { id: "finance", title: "Finance", icon: CreditCard },
    ],
  },
  {
    heading: "Developers",
    items: [
      { id: "api", title: "API Keys", icon: Terminal },
      { id: "webhooks", title: "Webhooks", icon: Blocks },
    ],
  },
];

export const mockBottomItems: NavItemData[] = [
  { id: "settings", title: "Settings", icon: Settings, shortcut: "⌘," },
  { id: "logout", title: "Log out", icon: LogOut },
];

function WorkspaceSwitcher({
  selected,
  onSelect,
  options = ["Acme Corp", "Personal Workspace", "Client Sandbox"],
  plan = "Pro Plan",
  logo,
  createLabel = "Create Workspace",
}: {
  selected?: string;
  onSelect?: (ws: string) => void;
  options?: (string | WorkspaceOption)[];
  plan?: string;
  logo?: React.ReactNode;
  createLabel?: string;
}) {
  const [isOpen, setIsOpen] = useState(false);
  const rows = options.map(asOption);
  const [internalSelected, setInternalSelected] = useState(rows[0]?.label ?? "Acme Corp");

  const current = selected || internalSelected;
  const handleSelect = onSelect || setInternalSelected;
  /* The line under the name says what the selected row IS, falling back to the paste's `plan`. */
  const subtitle = rows.find((row) => row.label === current)?.note ?? plan;

  return (
    <div className="relative">
      <div
        onClick={() => setIsOpen(!isOpen)}
        className="flex items-center justify-between px-2 py-2 mb-4 rounded-lg hover:bg-black/5 dark:hover:bg-white/5 cursor-pointer transition-colors select-none group"
      >
        <div className="flex items-center gap-3">
          {/*
            The brand's one slot. `logo` replaces the tile entirely rather than
            sitting inside it: the mark carries its own glow on transparency, and
            a `bg-primary` plate behind it would put a near-white square around a
            cyan light. Without a logo this is the paste, character for character.
          */}
          {logo ?? (
            <div className="w-8 h-8 rounded-[6px] bg-primary text-primary-foreground flex items-center justify-center font-semibold text-[13px] shadow-sm">
              {current.charAt(0)}
            </div>
          )}
          <div className="flex flex-col overflow-hidden">
            <span className="text-[13px] font-medium leading-none mb-1 text-foreground truncate max-w-[120px]">
              {current}
            </span>
            <span className="text-[11px] text-muted-foreground leading-none">{subtitle}</span>
          </div>
        </div>
        <ChevronDown
          className="w-4 h-4 text-muted-foreground/50 group-hover:text-foreground/70 transition-colors shrink-0"
          strokeWidth={1.5}
        />
      </div>

      {isOpen && (
        <>
          <div className="fixed inset-0 z-40" onClick={() => setIsOpen(false)} />
          <div className="absolute top-[52px] left-0 w-full bg-card border border-border/50 rounded-lg shadow-xl z-50 py-1 flex flex-col gap-0.5 animate-in fade-in zoom-in-95 duration-100">
            {rows.map((row) => (
              <React.Fragment key={row.label}>
                <div
                  onClick={() => {
                    handleSelect(row.label);
                    setIsOpen(false);
                  }}
                  title={row.mode}
                  className={`px-3 py-2 mx-1 text-[13px] rounded-md cursor-pointer transition-colors flex items-center gap-2.5 ${current === row.label ? "bg-primary/10 text-primary font-medium" : "text-foreground/80 hover:bg-black/5 dark:hover:bg-white/5"}`}
                >
                  {/*
                    One 16px slot, whatever goes in it, so every label starts on
                    the same x. A ragged left edge is what makes a list of five
                    read as two lists.
                  */}
                  <span className="w-4 shrink-0 flex items-center justify-center">
                    {row.leading ?? (row.mode && <ModeDot mode={row.mode} />)}
                  </span>
                  <span className="truncate">{row.label}</span>
                </div>
                {row.separatorAfter && <div className="h-px bg-border/50 my-1 mx-2" />}
              </React.Fragment>
            ))}
            <div className="h-px bg-border/50 my-1 mx-2" />
            <div className="px-3 py-2 mx-1 text-[13px] text-muted-foreground hover:bg-black/5 dark:hover:bg-white/5 rounded-md cursor-pointer flex items-center gap-2 transition-colors">
              <span className="text-[16px] leading-none mb-0.5">+</span> {createLabel}
            </div>
          </div>
        </>
      )}
    </div>
  );
}

function NavItem({
  item,
  activeId,
  onSelect,
  level = 0,
}: {
  item: NavItemData;
  activeId: string;
  onSelect: (id: string) => void;
  level?: number;
}) {
  const isActive = activeId === item.id;
  const hasChildren = !!item.children;
  const [isOpen, setIsOpen] = useState(false);

  const handleClick = () => {
    if (hasChildren) {
      setIsOpen(!isOpen);
    } else {
      onSelect(item.id);
    }
  };

  return (
    <div className="flex flex-col w-full">
      <div
        className={`group flex items-center justify-between px-2.5 py-[7px] rounded-[6px] cursor-pointer transition-all duration-200 select-none
          ${
            isActive
              ? "bg-black/5 dark:bg-white/10 text-foreground font-medium"
              : "text-muted-foreground hover:bg-black/5 dark:hover:bg-white/5 hover:text-foreground/90"
          }
        `}
        style={{ paddingLeft: `${level * 12 + 10}px` }}
        onClick={handleClick}
      >
        <div className="flex items-center gap-2.5">
          <item.icon
            className={`w-[16px] h-[16px] transition-colors
              ${isActive ? "text-foreground" : "text-muted-foreground/70 group-hover:text-foreground/70"}
            `}
            strokeWidth={1.5}
          />
          <span className="text-[13px] tracking-wide truncate">{item.title}</span>
        </div>

        <div className="flex items-center gap-2">
          {item.shortcut && (
            <kbd className="hidden group-hover:inline-flex items-center justify-center h-5 px-1.5 text-[10px] font-medium font-mono text-muted-foreground/60 bg-background/50 border border-border/50 rounded-[4px] shadow-xs">
              {item.shortcut}
            </kbd>
          )}
          {item.badge && (
            <span className="flex items-center justify-center min-w-[20px] h-5 px-1.5 text-[10px] font-medium rounded-full bg-primary/10 text-primary">
              {item.badge}
            </span>
          )}
          {hasChildren && (
            <ChevronRight
              className={`w-3.5 h-3.5 text-muted-foreground/50 transition-transform duration-200 ${isOpen ? "rotate-90" : ""}`}
              strokeWidth={2}
            />
          )}
        </div>
      </div>

      {hasChildren && (
        <div
          className={`grid transition-[grid-template-rows,opacity] duration-300 ease-in-out ${
            isOpen ? "grid-rows-[1fr] opacity-100" : "grid-rows-[0fr] opacity-0"
          }`}
        >
          <div className="overflow-hidden min-h-0 relative flex flex-col gap-0.5 mt-0.5">
            <div
              className="absolute top-0 bottom-0 border-l border-black/5 dark:border-white/5"
              style={{ left: `${level * 12 + 17.5}px` }}
            />
            {item.children!.map((child) => (
              <NavItem
                key={child.id}
                item={child}
                activeId={activeId}
                onSelect={onSelect}
                level={level + 1}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

export function SidebarNav({
  className = "",
  activeId,
  onSelect,
  activeWorkspace,
  onWorkspaceSelect,
  groups = mockNavGroups,
  bottomItems = mockBottomItems,
  workspaces,
  plan,
  logo,
  createLabel,
}: {
  className?: string;
  activeId?: string;
  onSelect?: (id: string) => void;
  activeWorkspace?: string;
  onWorkspaceSelect?: (ws: string) => void;
  groups?: NavGroupData[];
  bottomItems?: NavItemData[];
  workspaces?: (string | WorkspaceOption)[];
  plan?: string;
  logo?: React.ReactNode;
  createLabel?: string;
}) {
  const [internalId, setInternalId] = useState("home");
  const currentId = activeId !== undefined ? activeId : internalId;
  const handleSelect = onSelect || setInternalId;

  return (
    <div
      className={`flex flex-col w-[260px] h-full bg-card/50 border-r border-border/50 p-3 font-sans ${className}`}
    >
      <WorkspaceSwitcher
        selected={activeWorkspace}
        onSelect={onWorkspaceSelect}
        options={workspaces}
        plan={plan}
        logo={logo}
        createLabel={createLabel}
      />

      <div className="flex-1 overflow-y-auto [&::-webkit-scrollbar]:hidden [-ms-overflow-style:none] [scrollbar-width:none] flex flex-col gap-4 mt-2">
        {groups.map((group, idx) => (
          <div key={idx} className="flex flex-col gap-0.5">
            {group.heading && (
              <span className="px-2.5 mb-1 text-[11px] font-semibold tracking-wider text-muted-foreground/50 uppercase">
                {group.heading}
              </span>
            )}
            {group.items.map((item) => (
              <NavItem key={item.id} item={item} activeId={currentId} onSelect={handleSelect} />
            ))}
          </div>
        ))}
      </div>

      <div className="mt-auto pt-4 border-t border-border/50 flex flex-col gap-0.5">
        {bottomItems.map((item) => (
          <NavItem key={item.id} item={item} activeId={currentId} onSelect={handleSelect} />
        ))}
      </div>
    </div>
  );
}

export const flattenItems = (items: NavItemData[]): NavItemData[] =>
  items.reduce((acc, item) => {
    acc.push(item);
    if (item.children) acc.push(...flattenItems(item.children));
    return acc;
  }, [] as NavItemData[]);

export default function SidebarNavPreview({
  groups = mockNavGroups,
  bottomItems = mockBottomItems,
  workspaces = ["Acme Corp", "Personal Workspace", "Client Sandbox"],
  plan,
  logo,
  createLabel,
}: {
  groups?: NavGroupData[];
  bottomItems?: NavItemData[];
  workspaces?: (string | WorkspaceOption)[];
  plan?: string;
  logo?: React.ReactNode;
  createLabel?: string;
} = {}) {
  const [isOpen, setIsOpen] = useState(true);
  const [activeId, setActiveId] = useState(groups[0]?.items[1]?.id ?? "home");
  const [activeWorkspace, setActiveWorkspace] = useState(asOption(workspaces[0] ?? "Acme Corp").label);
  const [isSearchOpen, setIsSearchOpen] = useState(false);

  const flat = flattenItems([...groups.flatMap((g) => g.items), ...bottomItems]);
  const activeItem = flat.find((i) => i.id === activeId);
  const activeTitle = activeItem ? activeItem.title : "Dashboard";

  const handleSelect = (id: string) => {
    if (id === "search") {
      setIsSearchOpen(true);
      return;
    }
    setActiveId(id);
  };

  return (
    <div className="flex flex-col items-center justify-center w-full min-h-[700px] bg-background p-4 md:p-8">
      <div className="relative w-full max-w-4xl h-[700px] bg-card rounded-xl border border-border/50 flex overflow-hidden shadow-sm ring-1 ring-black/5 dark:ring-white/5">
        <div
          className={`h-full transition-all duration-300 ease-in-out shrink-0 overflow-hidden bg-card/50 border-r border-border/50 ${
            isOpen ? "w-[260px] opacity-100" : "w-0 opacity-0 border-none"
          }`}
        >
          <SidebarNav
            className="w-[260px] border-none bg-transparent"
            activeId={activeId}
            onSelect={handleSelect}
            activeWorkspace={activeWorkspace}
            onWorkspaceSelect={setActiveWorkspace}
            groups={groups}
            bottomItems={bottomItems}
            workspaces={workspaces}
            plan={plan}
            logo={logo}
            createLabel={createLabel}
          />
        </div>

        <div className="flex-1 bg-black/[0.02] dark:bg-white/[0.02] flex flex-col min-w-0 transition-all duration-300">
          <div className="h-14 border-b border-border/50 flex items-center px-4 justify-between bg-card shrink-0">
            <div className="flex items-center gap-3">
              <button
                onClick={() => setIsOpen(!isOpen)}
                className="p-1.5 rounded-md text-muted-foreground hover:bg-black/5 dark:hover:bg-white/5 hover:text-foreground transition-colors"
              >
                {isOpen ? (
                  <PanelLeftClose className="w-[18px] h-[18px]" strokeWidth={1.5} />
                ) : (
                  <PanelLeftOpen className="w-[18px] h-[18px]" strokeWidth={1.5} />
                )}
              </button>
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <span className="truncate">{activeWorkspace}</span>
                <span>/</span>
                <span className="font-medium text-foreground truncate">{activeTitle}</span>
              </div>
            </div>

            <div className="flex items-center gap-3">
              <div className="w-64 h-8 bg-black/5 dark:bg-white/5 rounded-md hidden md:block" />
              <div className="w-8 h-8 bg-primary/10 rounded-full border border-primary/20" />
            </div>
          </div>

          <div className="p-6 md:p-8 overflow-y-auto [&::-webkit-scrollbar]:hidden [-ms-overflow-style:none] [scrollbar-width:none]">
            <div className="flex items-center justify-between mb-8">
              <div className="w-48 h-8 bg-black/5 dark:bg-white/5 rounded-md" />
            </div>

            <div className="grid grid-cols-1 md:grid-cols-2 gap-6 mb-6">
              <div className="h-32 bg-card rounded-xl border border-border/50 shadow-sm" />
              <div className="h-32 bg-card rounded-xl border border-border/50 shadow-sm" />
            </div>

            <div className="w-full bg-card rounded-xl border border-border/50 shadow-sm p-6">
              <div className="w-1/3 h-5 bg-black/5 dark:bg-white/5 rounded-md mb-6" />
              <div className="w-full h-[1px] bg-border/50 mb-6" />

              <div className="flex flex-col gap-4">
                <div className="w-full h-12 bg-black/5 dark:bg-white/5 rounded-lg" />
                <div className="w-full h-12 bg-black/5 dark:bg-white/5 rounded-lg" />
                <div className="w-full h-12 bg-black/5 dark:bg-white/5 rounded-lg" />
                <div className="w-full h-12 bg-black/5 dark:bg-white/5 rounded-lg" />
              </div>
            </div>
          </div>
        </div>

        {isSearchOpen && (
          <div className="absolute inset-0 z-50 flex items-start justify-center pt-[15vh] bg-background/40 backdrop-blur-sm px-4">
            <div className="absolute inset-0" onClick={() => setIsSearchOpen(false)} />
            <div className="relative w-full max-w-xl bg-card border border-border/50 rounded-xl shadow-2xl overflow-hidden animate-in fade-in zoom-in-95 duration-200">
              <div className="flex items-center px-4 border-b border-border/50">
                <Search
                  className="w-[18px] h-[18px] text-muted-foreground/70 mr-3 shrink-0"
                  strokeWidth={1.5}
                />
                <input
                  autoFocus
                  className="flex-1 bg-transparent py-4 outline-none text-[14px] text-foreground placeholder:text-muted-foreground/50"
                  placeholder="Search projects, docs, or actions..."
                />
                <kbd
                  onClick={() => setIsSearchOpen(false)}
                  className="hidden sm:inline-flex items-center justify-center h-5 px-1.5 ml-2 text-[10px] font-medium font-mono text-muted-foreground/70 bg-black/5 dark:bg-white/10 border border-black/10 dark:border-white/10 rounded-[4px] cursor-pointer hover:text-foreground hover:bg-black/10 dark:hover:bg-white/20 transition-colors"
                >
                  ESC
                </kbd>
                <button
                  onClick={() => setIsSearchOpen(false)}
                  className="ml-3 p-1 rounded-md text-muted-foreground/70 hover:bg-black/5 dark:hover:bg-white/10 hover:text-foreground transition-colors"
                >
                  <X className="w-[18px] h-[18px]" strokeWidth={1.5} />
                </button>
              </div>
              <div className="p-2 py-8 flex flex-col items-center justify-center">
                <Command className="w-6 h-6 text-muted-foreground/30 mb-2" strokeWidth={1.5} />
                <p className="text-[13px] text-muted-foreground font-medium">
                  Type a command or search...
                </p>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

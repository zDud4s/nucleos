import type { ReactNode } from "react";
import { useEffect, useState } from "react";

import { WorkflowCanvas } from "../canvas/WorkflowCanvas";
import type { GraphEdge, GraphNode } from "../data/workflow-graph";
import {
  CommandDialog,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/ui/vendor/command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/ui/vendor/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/ui/vendor/dropdown-menu";

/**
 * The surfaces the CSP gate puts under the production policy, and why each one is here.
 *
 * **This is a list of LIBRARIES, not a list of screens**, and the distinction is what keeps it
 * short enough to stay true. A Content-Security-Policy refusal is a property of how a dependency
 * writes style — a `<style>` element it builds at runtime, or a `style=` attribute it puts into
 * markup — so covering one screen that uses xyflow covers every screen that uses xyflow. Four
 * libraries in this app can write style that way, and every one of them has a surface below.
 *
 * The list is meant to be annoying in the same way `app/nav.test.ts` is: **adding a dependency
 * that renders should mean adding a row here.** Nothing enforces that, and nothing can — the gate
 * cannot know what a new package does before somebody thinks about it. What it can do is make the
 * omission visible, which is why each row says what it is standing in for rather than just naming
 * a component.
 *
 * Two absences, both deliberate:
 *
 * - **`FleetCanvas`** is not here. It is xyflow, and xyflow is already covered by the workflow
 *   canvas; a second surface of the same library buys a second chance to be slow and no new
 *   information.
 * - **`@visx/*`** is not here because nothing in `src/` imports it yet. It was installed in slice
 *   0 for charts that have not landed. When the first one does, it wants a row — a chart library
 *   that emits a `<style>` for its tooltips is exactly this gate's case.
 */

/** One thing under test: mounted alone, on its own page load, so a refusal has one suspect. */
export interface Surface {
  /** What the driver passes as `?surface=`. */
  name: string;
  /** The library this stands in for, and what it does that a CSP could refuse. */
  why: string;
  /**
   * Mounted with `done` to call when it has finished exercising itself.
   *
   * The exercising lives in the surface and not in the driver on purpose: what counts as "opened"
   * or "animating" is a fact about the component, and a driver that clicked buttons by their text
   * would go quietly green the day a label changed.
   */
  Component: (props: { done: () => void }) => ReactNode;
}

/** Long enough for a spring to run and a portal to settle; short enough to run four of them. */
const SETTLE = 1200;

function useDoneAfter(done: () => void, delay = SETTLE) {
  useEffect(() => {
    const timer = window.setTimeout(done, delay);
    return () => window.clearTimeout(timer);
  }, [done, delay]);
}

/* -------------------------------------------------------------------------- */

const GRAPH_NODES: GraphNode[] = [
  {
    id: "plan",
    type: "agent",
    role: "plain",
    label: "plan",
    disabled: false,
    overridden: true,
    fields: [{ name: "model", value: "opus", origin: "sonnet" }],
  },
  { id: "build", type: "agent", role: "plain", label: "build", disabled: false, overridden: false, fields: [] },
  {
    id: "gate",
    type: "command",
    role: "gate",
    label: "gate",
    disabled: false,
    overridden: false,
    fields: [{ name: "command", value: "cargo test" }],
  },
  { id: "each", type: "fan", role: "plain", label: "each package", disabled: false, overridden: false, fields: [] },
  { id: "review", type: "decision", role: "plain", label: "review", disabled: true, overridden: true, fields: [] },
];

const GRAPH_EDGES: GraphEdge[] = [
  { from: "plan", to: "build" },
  { from: "build", to: "gate" },
  { from: "gate", to: "each", verdict: "pass" },
  { from: "gate", to: "build", verdict: "fail" },
  { from: "each", to: "review", when: "changed" },
];

/**
 * `motion` and `@xyflow/react`.
 *
 * All four node kinds and both edge kinds, because the edge labels go through xyflow's
 * `EdgeLabelRenderer` — a portal that positions with a transform, which is the shape most likely
 * to be written as a `style` attribute by a library that did it the other way. `running` is set
 * from the first frame so the spring is live, then cleared, so both branches of the `animate` prop
 * are exercised rather than only the resting one.
 */
function WorkflowSurface({ done }: { done: () => void }) {
  const [running, setRunning] = useState<string | null>("build");
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => {
    const stop = window.setTimeout(() => setRunning(null), 700);
    const pick = window.setTimeout(() => setSelected("gate"), 300);
    return () => {
      window.clearTimeout(stop);
      window.clearTimeout(pick);
    };
  }, []);
  useDoneAfter(done, 1800);

  return (
    <div className="p-6">
      <WorkflowCanvas
        nodes={GRAPH_NODES}
        edges={GRAPH_EDGES}
        running={running}
        selected={selected}
        onSelect={setSelected}
      />
    </div>
  );
}

/**
 * `radix-ui`, through the one thing in it that has already bitten this app.
 *
 * An open modal dialog is what turns on `react-remove-scroll`, and `react-remove-scroll` locks
 * scrolling by **injecting a `<style>` element**. It is the reason this gate exists in the shape
 * it does: it works in `tauri dev`, where `devCsp` allows inline style, and it is refused in a
 * packaged build. Mounted `open` from the first render rather than clicked open, because a
 * trigger is a screen detail and the lock is what is under test.
 */
function DialogSurface({ done }: { done: () => void }) {
  useDoneAfter(done);

  return (
    <Dialog open>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>An open modal dialog</DialogTitle>
          <DialogDescription>
            Open from the first render, so the scroll lock is engaged.
          </DialogDescription>
        </DialogHeader>
        <p>Body, long enough that the document behind it would scroll.</p>
      </DialogContent>
    </Dialog>
  );
}

/**
 * `radix-ui` again, on the other half of it: Popper, not Dialog.
 *
 * A dropdown positions its content by measuring the trigger and writing the result somewhere. If
 * that somewhere is a `style` attribute in markup rather than the CSSOM, `style-src-attr` refuses
 * it and the menu lands in the top-left corner of the window in a packaged build only. Worth its
 * own surface because it is a different code path from the dialog's, not a second dialog.
 */
function MenuSurface({ done }: { done: () => void }) {
  useDoneAfter(done);

  return (
    <div className="p-24">
      <DropdownMenu open>
        <DropdownMenuTrigger>a menu</DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuLabel>positioned by Popper</DropdownMenuLabel>
          <DropdownMenuSeparator />
          <DropdownMenuItem>first</DropdownMenuItem>
          <DropdownMenuItem>second</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

/**
 * `cmdk`, which is the ⌘K palette slice 6 put in front of every project command.
 *
 * It arrives wrapped in a Radix dialog, so this surface covers both at once — but it is not the
 * dialog surface repeated: cmdk does its own filtering and its own list virtualisation, and both
 * are places a library writes style per keystroke.
 */
function PaletteSurface({ done }: { done: () => void }) {
  useDoneAfter(done);

  return (
    <CommandDialog open>
      <CommandInput placeholder="type a command" />
      <CommandList>
        <CommandGroup heading="gates">
          <CommandItem>gate</CommandItem>
          <CommandItem>fmt</CommandItem>
          <CommandItem>typecheck</CommandItem>
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  );
}

export const SURFACES: Surface[] = [
  {
    name: "workflow",
    why: "motion (springs, layout) and @xyflow/react (nodes, edges, the edge-label portal)",
    Component: WorkflowSurface,
  },
  {
    name: "dialog",
    why: "radix-ui's modal dialog, which turns on react-remove-scroll — the one that injects a <style>",
    Component: DialogSurface,
  },
  {
    name: "menu",
    why: "radix-ui's Popper positioning, a different path from the dialog's",
    Component: MenuSurface,
  },
  {
    name: "palette",
    why: "cmdk, the ⌘K palette, filtering and rendering inside a dialog",
    Component: PaletteSurface,
  },
];

"use client"

import * as React from "react"
import { Tabs as TabsPrimitive } from "radix-ui"

import { cn } from "@/lib/cn"

/*
  The registry's Tabs, in the shape `dialog.tsx` and `dropdown-menu.tsx` already
  established here: the primitive wrapped one-for-one, a `data-slot` on each
  part, `cn` so a caller can override, and no behaviour of our own. The value is
  Radix's roving focus and its `aria-controls` wiring — a set of buttons and a
  conditional would look the same and would not be a tab list to anything that
  is not a mouse.

  **The classes are `ui-tab*` and not Tailwind**, which is where this file
  departs from its neighbours. `base.css` resets only `font` and `color` on a
  `button` — never `background` — and this app's `tailwind.css` is trimmed, so a
  trigger carrying no background of its own renders on the UA's light
  `ButtonFace` with near-white inherited text: light boxes with unreadable
  labels, on a dark page. It typechecked, every test passed, and it was visible
  only in a screenshot. The rules live in `ui.css` and say the same at length.

  Covered by the `menu` surface of the CSP gate rather than by one of its own:
  Tabs uses the same roving-focus machinery as the menu and does not position
  with Popper, so it emits no style the menu surface does not already prove.
*/

function Tabs({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Root>) {
  return <TabsPrimitive.Root data-slot="tabs" className={cn("ui-tabs", className)} {...props} />
}

function TabsList({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.List>) {
  return (
    <TabsPrimitive.List data-slot="tabs-list" className={cn("ui-tab-list", className)} {...props} />
  )
}

function TabsTrigger({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Trigger>) {
  return (
    <TabsPrimitive.Trigger data-slot="tabs-trigger" className={cn("ui-tab", className)} {...props} />
  )
}

function TabsContent({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Content>) {
  return (
    <TabsPrimitive.Content
      data-slot="tabs-content"
      className={cn("ui-tab-panel", className)}
      {...props}
    />
  )
}

export { Tabs, TabsContent, TabsList, TabsTrigger }

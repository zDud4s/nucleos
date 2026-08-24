"use client"

import * as React from "react"
import { Tabs as TabsPrimitive } from "radix-ui"

import { cn } from "@/lib/cn"

/*
  The registry's Tabs, in the shape `dialog.tsx` and `dropdown-menu.tsx` already
  established here: the primitive wrapped one-for-one, a `data-slot` on each
  part, Tailwind through `cn` so a caller can override, and no behaviour of our
  own. The value is Radix's roving focus and its `aria-controls` wiring — a set
  of buttons and a conditional would look the same and would not be a tab list
  to anything that is not a mouse.

  Covered by the `menu` surface of the CSP gate rather than by one of its own:
  Tabs uses the same roving-focus machinery as the menu and does not position
  with Popper, so it emits no style the menu surface does not already prove.
*/

function Tabs({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Root>) {
  return (
    <TabsPrimitive.Root
      data-slot="tabs"
      className={cn("flex flex-col gap-4", className)}
      {...props}
    />
  )
}

function TabsList({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.List>) {
  return (
    <TabsPrimitive.List
      data-slot="tabs-list"
      className={cn(
        "inline-flex w-fit items-center gap-1 border-b border-border",
        className
      )}
      {...props}
    />
  )
}

function TabsTrigger({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Trigger>) {
  return (
    <TabsPrimitive.Trigger
      data-slot="tabs-trigger"
      className={cn(
        "inline-flex items-center gap-2 whitespace-nowrap border-b-2 border-transparent px-3 py-2 text-sm text-text-muted transition-colors hover:text-text focus-visible:outline-2 focus-visible:outline-focus-ring disabled:pointer-events-none disabled:opacity-50 data-[state=active]:border-accent data-[state=active]:text-text",
        className
      )}
      {...props}
    />
  )
}

function TabsContent({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Content>) {
  return (
    <TabsPrimitive.Content
      data-slot="tabs-content"
      className={cn("flex-1 outline-none", className)}
      {...props}
    />
  )
}

export { Tabs, TabsContent, TabsList, TabsTrigger }

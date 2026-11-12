"use client"

import * as React from "react"
import { Tabs as TabsPrimitive } from "radix-ui"

import { cn } from "@/lib/cn"
import { SlideMark } from "../SlideMark"

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

/*
  **The sliding underline needs to know which trigger is active, and a trigger cannot.** Radix
  writes `data-state` onto the DOM but exposes no value to React from inside `Trigger`. So `Tabs`
  keeps its own copy of the value, the way Radix's controllable state does — `value` when the
  caller controls it, else `defaultValue` followed through `onValueChange` — and hands it down with
  an id unique to this tab list, which is the `layoutId` the active trigger's mark shares. Radix
  still owns the real state; this copy only decides where the decorative mark is drawn, and the
  props the caller passes are untouched.
*/
const TabsMarkContext = React.createContext<{ value: string | undefined; id: string }>({
  value: undefined,
  id: "",
})

function Tabs({
  className,
  value,
  defaultValue,
  onValueChange,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Root>) {
  const id = React.useId()
  const [inner, setInner] = React.useState(defaultValue)
  const current = value ?? inner
  return (
    <TabsMarkContext.Provider value={{ value: current, id }}>
      <TabsPrimitive.Root
        data-slot="tabs"
        className={cn("ui-tabs", className)}
        value={value}
        defaultValue={defaultValue}
        onValueChange={(next) => {
          setInner(next)
          onValueChange?.(next)
        }}
        {...props}
      />
    </TabsMarkContext.Provider>
  )
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
  children,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Trigger>) {
  const mark = React.useContext(TabsMarkContext)
  return (
    <TabsPrimitive.Trigger data-slot="tabs-trigger" className={cn("ui-tab", className)} {...props}>
      {children}
      {mark.value === props.value ? <SlideMark layoutId={mark.id} kind="rule" /> : null}
    </TabsPrimitive.Trigger>
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

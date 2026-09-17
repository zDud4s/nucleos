/**
 * The design system's front door.
 *
 * Pages import from `../ui`, never from `../ui/Badge` — one import site per
 * page, and one place to see what the app is built out of. A primitive that is
 * not exported here does not exist as far as the rest of the shell is concerned.
 */
export { Badge, type BadgeProps, type BadgeTone } from "./Badge";
export { Button, type ButtonIntent, type ButtonProps, type ButtonVariant } from "./Button";
export { ConfirmButton, type ConfirmButtonProps } from "./ConfirmButton";
export { CopyButton, type CopyButtonProps } from "./CopyButton";
export { ConflictNote, type ConflictNoteProps } from "./ConflictNote";
export { CopyOnce, type CopyOnceProps } from "./CopyOnce";
export { Count, type CountProps } from "./Count";
export { Crumb, type CrumbProps } from "./Crumb";
export { ErrorNote, type ErrorNoteProps } from "./ErrorNote";
export { Field, type FieldProps } from "./Field";
export { IconButton, type IconButtonProps } from "./IconButton";
export { Inset, type InsetAs, type InsetProps } from "./Inset";
/**
 * Two primitives in one module — the second recorded exception to one
 * primitive per file. See the header of `Meter.tsx`: the pair exists for the
 * contrast between a ceiling something occupies and a rule applied per task,
 * and splitting them is how the next ceiling gets drawn as the wrong one.
 */
export { LimitChip, Meter, usd, type LimitChipProps, type MeterProps, type MeterTone } from "./Meter";
export { ModeSwitch, type ModeSwitchProps, type SwitchMode } from "./ModeSwitch";
export { PageHeader, type PageHeaderProps } from "./PageHeader";
/**
 * The one palette, and the one place the `Ctrl K` chord is bound.
 *
 * A page does not open a palette of its own: it contributes a group through
 * `usePaletteGroup` and the shell's provider draws it. `SHORTCUT_HINT` is here
 * so a page can print the chord beside a button without deciding what it is.
 */
export {
  PaletteProvider,
  SHORTCUT_HINT,
  usePaletteGroup,
  usePaletteOpen,
  usePaletteQuery,
  type PaletteGroup,
  type PaletteItem,
} from "./Palette";
export { Panel, type PanelProps, type PanelVariant } from "./Panel";
export { Quiet, type QuietProps } from "./Quiet";
/**
 * Three formatters in one module — the recorded exception to one primitive per
 * file. See the header of `readings.tsx` for why they travel together.
 */
export {
  ContextMeter,
  CostLine,
  RelativeTime,
  money,
  relativeText,
  type ContextMeterProps,
  type CostLineProps,
  type RelativeTimeProps,
} from "./readings";
export { RefusalNote, type RefusalNoteProps } from "./RefusalNote";
/**
 * A container and its item in one module — the third recorded exception to one
 * primitive per file. See the header of `Rows.tsx`: a row exists only inside
 * one of these, and it paints the fill that keeps the container's hairline
 * ground from showing through, so the two are one mechanism written twice.
 */
export { Row, Rows, type RowProps, type RowsProps } from "./Rows";
export { Section, type SectionProps } from "./Section";
export { SectionTitle, type SectionTitleProps } from "./SectionTitle";
export { SlotPips, type SlotPipsProps } from "./SlotPips";
export { Sparkline, type SparklineProps } from "./Sparkline";
export { StaleNote, type StaleNoteProps } from "./StaleNote";
export { StatCard, type StatCardProps } from "./StatCard";
export { StateBadge, type StateBadgeProps } from "./StateBadge";
export { Teach, type TeachProps } from "./Teach";
export { Well, type WellProps } from "./Well";
export { Who, type WhoProps } from "./Who";
export { readState, type StateDomain, type StateReading } from "./state-map";
/**
 * The Feed's two readings of a kind — its lane on the time axis and its weight — kept beside the
 * map because both are claims about núcleo kinds, and a page may not author a tone.
 */
export {
  FEED_LANES,
  feedGravityOf,
  feedGravityTone,
  feedKindLeavesOpen,
  feedLaneOf,
  feedMarkTone,
  type FeedGravity,
  type FeedLane,
  type FeedLaneInfo,
} from "./lanes";
/**
 * Radix Tabs, re-exported from the front door like every other primitive.
 *
 * `ui/vendor/` is where a registry component is *kept*, not where a page reads
 * it from — pages import from `../ui`, and the vendor path is an implementation
 * detail of this directory. The rule is the same one the header states; it is
 * repeated here because a vendored component is the case most likely to be
 * imported by its own path out of habit.
 */
export { Tabs, TabsContent, TabsList, TabsTrigger } from "./vendor/tabs";

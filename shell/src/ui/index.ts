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
export { CopyOnce, type CopyOnceProps } from "./CopyOnce";
export { ErrorNote, type ErrorNoteProps } from "./ErrorNote";
/**
 * Two primitives in one module — the second recorded exception to one
 * primitive per file. See the header of `Meter.tsx`: the pair exists for the
 * contrast between a ceiling something occupies and a rule applied per task,
 * and splitting them is how the next ceiling gets drawn as the wrong one.
 */
export { LimitChip, Meter, usd, type LimitChipProps, type MeterProps, type MeterTone } from "./Meter";
export { PageHeader, type PageHeaderProps } from "./PageHeader";
export { Panel, type PanelProps, type PanelVariant } from "./Panel";
/**
 * Three formatters in one module — the recorded exception to one primitive per
 * file. See the header of `readings.tsx` for why they travel together.
 */
export {
  ContextMeter,
  CostLine,
  RelativeTime,
  type ContextMeterProps,
  type CostLineProps,
  type RelativeTimeProps,
} from "./readings";
export { RefusalNote, type RefusalNoteProps } from "./RefusalNote";
export { Sparkline, type SparklineProps } from "./Sparkline";
export { StaleNote, type StaleNoteProps } from "./StaleNote";
export { StatCard, type StatCardProps } from "./StatCard";
export { StateBadge, type StateBadgeProps } from "./StateBadge";
export { Teach, type TeachProps } from "./Teach";
export { Who, type WhoProps } from "./Who";
export { readState, type StateDomain, type StateReading } from "./state-map";
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

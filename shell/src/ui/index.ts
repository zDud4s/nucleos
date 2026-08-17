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
export { ErrorNote, type ErrorNoteProps } from "./ErrorNote";
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
export { StaleNote, type StaleNoteProps } from "./StaleNote";
export { StatCard, type StatCardProps } from "./StatCard";
export { StateBadge, type StateBadgeProps } from "./StateBadge";
export { Teach, type TeachProps } from "./Teach";
export { readState, type StateDomain, type StateReading } from "./state-map";

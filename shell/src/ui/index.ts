/**
 * The design system's front door.
 *
 * Pages import from `../ui`, never from `../ui/Badge` — one import site per
 * page, and one place to see what the app is built out of. A primitive that is
 * not exported here does not exist as far as the rest of the shell is concerned.
 */
export { AreaChart, type AreaChartProps } from "./AreaChart";
export { Badge, type BadgeProps, type BadgeTone } from "./Badge";
export { seriesClass, type ChartDatum, type ChartKey, type ChartPart } from "./chart-series";
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
export { LimitChip, Meter, NEAR_CEILING, ceilingShare, usd, type LimitChipProps, type MeterProps, type MeterTone } from "./Meter";
export { Modal, type ModalProps, type ModalSize } from "./Modal";
export { TimeField, type TimeFieldProps } from "./TimeField";
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
export { ProviderMark, type ProviderMarkProps } from "./ProviderMark";
export { QueryState, type QueryLike, type QueryStateProps } from "./QueryState";
export { Quiet, QuietTips, type QuietProps } from "./Quiet";
export { Ring, type RingProps, type RingTrack } from "./Ring";
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
export {
  RunPipeline,
  runStages,
  type RunPipelineProps,
  type RunShape,
  type Stage,
  type StageKey,
} from "./RunPipeline";
/**
 * A container and its item in one module — the third recorded exception to one
 * primitive per file. See the header of `Rows.tsx`: a row exists only inside
 * one of these, and it paints the fill that keeps the container's hairline
 * ground from showing through, so the two are one mechanism written twice.
 */
export { Row, Rows, type RowProps, type RowsProps } from "./Rows";
export { Section, type SectionProps } from "./Section";
export { SectionTitle, type SectionTitleProps } from "./SectionTitle";
export { ShareBar, type ShareBarProps, type ShareItem } from "./ShareBar";
export { SlotPips, type SlotPipsProps } from "./SlotPips";
export { Skeleton, type SkeletonProps } from "./Skeleton";
export { Sparkline, type SparklineProps } from "./Sparkline";
export { StaleNote, type StaleNoteProps } from "./StaleNote";
export { StatCard, type StatCardProps } from "./StatCard";
export { StateBadge, type StateBadgeProps } from "./StateBadge";
export { StepLine, type StepLineProps, type StepPoint, type StepSeries } from "./StepLine";
export { Teach, type TeachProps } from "./Teach";
export { Tip, type TipProps, type TipState } from "./Tip";
export { Term, type TermProps } from "./Term";
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
export { Slider, type SliderProps } from "./Slider";
export { Tabs, TabsContent, TabsList, TabsTrigger } from "./vendor/tabs";

export { SectionNav, type SectionNavItem, type SectionNavProps } from "./SectionNav";
export { Disclosure, type DisclosureProps } from "./Disclosure";
export { useUndoToast, UNDO_MS, type UndoToast, type UndoToastOptions } from "./useUndoToast";

export { Chip } from "./Chip";
export type { ChipProps } from "./Chip";
export { FilterBar } from "./FilterBar";
export type { FilterBarProps } from "./FilterBar";
export { ListFilter } from "./ListFilter";
export type { ListFilterProps, ListFilterChip } from "./ListFilter";

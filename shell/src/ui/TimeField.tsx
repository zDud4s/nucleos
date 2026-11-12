import { useEffect, useId, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type RefObject } from "react";
import { ClockIcon } from "lucide-react";
import { Popover as PopoverPrimitive } from "radix-ui";

export interface TimeFieldProps {
  /** `"HH:MM"`, 24-hour. A minute off the five-minute step still shows; it just has no row to light. */
  value: string;
  onChange: (value: string) => void;
  /** The control's accessible name, e.g. "Starts". The time is appended to it when it is read. */
  label: string;
  /**
   * The span that matters — working hours, usually — as `"HH:MM"` bounds. Hours outside it are
   * drawn quieter, never disabled: an evening call is unusual, not forbidden.
   */
  within?: { from: string; to: string };
}

const MINUTE_STEP = 5;
const HOURS = Array.from({ length: 24 }, (_, hour) => hour);
const MINUTES = Array.from({ length: 60 / MINUTE_STEP }, (_, index) => index * MINUTE_STEP);
/** How long a column must sit still before the row under its centre line is taken. */
const SETTLE_MS = 120;

const pad = (value: number) => String(value).padStart(2, "0");

function partsOf(value: string): { hour: number; minute: number } | null {
  const parsed = /^(\d{2}):(\d{2})$/.exec(value);
  if (parsed === null) return null;
  const hour = Number(parsed[1]);
  const minute = Number(parsed[2]);
  return hour < 24 && minute < 60 ? { hour, minute } : null;
}

/**
 * A time of day, picked from two scrolling columns — hours, then minutes.
 *
 * For a form that already knows WHICH day — a draft opened from a calendar cell — and only needs
 * WHEN in it. `datetime-local` asks for both and so asks again for the day the person just clicked,
 * and its picker is the browser's: a different look on every engine and none of this app's.
 *
 * Each column scrolls on its own and snaps a row to the centre line; the row that settles there is
 * the value, so a flick of the wheel is the whole gesture. A click takes a row directly. The
 * keyboard: on the closed control the up and down arrows nudge by five minutes without opening
 * anything; open, up/down step the focused column, left/right cross to the other one, and Enter or
 * Escape closes.
 *
 * Built on Radix's popover for the same reasons `Modal` is on Radix's dialog — focus moves in and
 * back, Escape and a click outside close it — and it nests inside a `Modal` without either one
 * stealing the other's Escape.
 */
export function TimeField({ value, onChange, label, within }: TimeFieldProps) {
  const [open, setOpen] = useState(false);
  const parts = partsOf(value);
  const hour = parts?.hour ?? 9;
  const minute = parts?.minute ?? 0;
  const from = within === undefined ? null : partsOf(within.from);
  const to = within === undefined ? null : partsOf(within.to);
  const hourColumn = useRef<HTMLDivElement>(null);

  function set(nextHour: number, nextMinute: number) {
    onChange(`${pad(nextHour)}:${pad(nextMinute)}`);
  }

  function nudge(event: ReactKeyboardEvent<HTMLButtonElement>) {
    const by = event.key === "ArrowUp" ? MINUTE_STEP : event.key === "ArrowDown" ? -MINUTE_STEP : 0;
    if (by === 0 || open) return;
    event.preventDefault();
    // Snap to the step first, so a nudge from 09:07 lands on 09:10 rather than 09:12.
    const total = hour * 60 + minute;
    const snapped =
      by > 0
        ? Math.floor(total / MINUTE_STEP) * MINUTE_STEP + MINUTE_STEP
        : Math.ceil(total / MINUTE_STEP) * MINUTE_STEP - MINUTE_STEP;
    const wrapped = (snapped + 24 * 60) % (24 * 60);
    set(Math.floor(wrapped / 60), wrapped % 60);
  }

  const quiet = (candidate: number) =>
    from !== null && to !== null && (candidate < from.hour || candidate >= to.hour + (to.minute > 0 ? 1 : 0));

  return (
    <PopoverPrimitive.Root open={open} onOpenChange={setOpen}>
      <PopoverPrimitive.Trigger className="ui-time-trigger" aria-label={`${label}, ${value}`} onKeyDown={nudge}>
        <ClockIcon className="ui-time-glyph" strokeWidth={1.5} aria-hidden="true" />
        <span className="ui-time-value">{value}</span>
      </PopoverPrimitive.Trigger>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          className="ui-time-popover"
          align="start"
          sideOffset={6}
          collisionPadding={12}
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            hourColumn.current?.focus();
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              setOpen(false);
            }
          }}
        >
          <div className="ui-time-columns">
            <Column
              name="Hour"
              items={HOURS}
              chosen={hour}
              quiet={quiet}
              onChoose={(next) => set(next, minute)}
              columnRef={hourColumn}
            />
            <span className="ui-time-colon" aria-hidden="true">
              :
            </span>
            <Column name="Minute" items={MINUTES} chosen={minute} onChoose={(next) => set(hour, next)} />
          </div>
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}

/**
 * One scrolling column: a listbox whose focus stays on the column, the chosen row named by
 * `aria-activedescendant`, so up/down step the value rather than walking a tab order of buttons.
 */
function Column({
  name,
  items,
  chosen,
  quiet,
  onChoose,
  columnRef,
}: {
  name: string;
  items: number[];
  chosen: number;
  quiet?: (item: number) => boolean;
  onChoose: (item: number) => void;
  columnRef?: RefObject<HTMLDivElement | null>;
}) {
  const own = useRef<HTMLDivElement>(null);
  const ref = columnRef ?? own;
  const id = useId();
  const settle = useRef<number | undefined>(undefined);
  /* Set while the column scrolls itself to a value, so that scroll is not read back as a choice. */
  const steering = useRef(false);
  const index = items.indexOf(chosen);

  function row(at: number): HTMLElement | null {
    return ref.current?.querySelector<HTMLElement>(`[data-index="${at}"]`) ?? null;
  }

  function centre(at: number, smooth: boolean) {
    const column = ref.current;
    const target = row(at);
    if (column === null || target === null) return;
    steering.current = true;
    const top = target.offsetTop - column.clientHeight / 2 + target.offsetHeight / 2;
    // jsdom has no `scrollTo` on elements; the property is the same jump without the glide.
    if (typeof column.scrollTo === "function") column.scrollTo({ top, behavior: smooth ? "smooth" : "auto" });
    else column.scrollTop = top;
    window.setTimeout(() => (steering.current = false), smooth ? 400 : 0);
  }

  // On open, and whenever the value changes from outside the wheel, bring it to the centre line.
  useEffect(() => {
    if (index >= 0) centre(index, false);
  }, []);

  function onScroll() {
    if (steering.current) return;
    window.clearTimeout(settle.current);
    settle.current = window.setTimeout(() => {
      const column = ref.current;
      if (column === null) return;
      const middle = column.scrollTop + column.clientHeight / 2;
      let nearest = 0;
      let distance = Infinity;
      items.forEach((_, at) => {
        const target = row(at);
        if (target === null) return;
        const gap = Math.abs(target.offsetTop + target.offsetHeight / 2 - middle);
        if (gap < distance) {
          distance = gap;
          nearest = at;
        }
      });
      if (items[nearest] !== chosen) onChoose(items[nearest]);
    }, SETTLE_MS);
  }

  function onKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    const by = event.key === "ArrowUp" ? -1 : event.key === "ArrowDown" ? 1 : 0;
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault();
      const columns = [...(ref.current?.parentElement?.querySelectorAll<HTMLElement>('[role="listbox"]') ?? [])];
      const here = columns.indexOf(ref.current as HTMLElement);
      columns[here + (event.key === "ArrowLeft" ? -1 : 1)]?.focus();
      return;
    }
    if (by === 0) return;
    event.preventDefault();
    const next = Math.min(items.length - 1, Math.max(0, (index < 0 ? 0 : index) + by));
    onChoose(items[next]);
    centre(next, true);
  }

  return (
    <div
      ref={ref}
      className="ui-time-column"
      role="listbox"
      aria-label={name}
      tabIndex={0}
      aria-activedescendant={index >= 0 ? `${id}-${index}` : undefined}
      onScroll={onScroll}
      onKeyDown={onKeyDown}
    >
      {items.map((item, at) => {
        const classes = ["ui-time-cell"];
        if (quiet?.(item)) classes.push("ui-time-cell-outside");
        return (
          <div
            key={item}
            id={`${id}-${at}`}
            role="option"
            aria-selected={item === chosen}
            data-index={at}
            className={classes.join(" ")}
            onClick={() => {
              onChoose(item);
              centre(at, true);
            }}
          >
            {pad(item)}
          </div>
        );
      })}
    </div>
  );
}

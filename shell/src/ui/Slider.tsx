import { Slider as SliderPrimitive } from "radix-ui";

export interface SliderProps {
  /** The named levels, weakest first. The slider moves between their indices. */
  steps: string[];
  /** Index into `steps`. */
  value: number;
  onChange: (index: number) => void;
  /** Accessible name of the thumb. */
  label: string;
  disabled?: boolean;
}

/**
 * A discrete slider over named levels, the effort dial.
 *
 * Index-based on purpose: the levels differ per model and are strings the daemon owns, so this
 * primitive knows only their order and names. Each level is printed under the track so the dial
 * never needs a legend.
 */
export function Slider({ steps, value, onChange, label, disabled }: SliderProps) {
  const max = Math.max(steps.length - 1, 0);
  const at = Math.min(Math.max(value, 0), max);
  return (
    <div className="ui-slider-wrap" data-disabled={disabled ? "" : undefined}>
      <SliderPrimitive.Root
        className="ui-slider"
        min={0}
        max={max}
        step={1}
        value={[at]}
        disabled={disabled}
        onValueChange={(v) => {
          const next = v[0];
          if (next !== undefined && next !== at) onChange(next);
        }}
      >
        <SliderPrimitive.Track className="ui-slider-track">
          <SliderPrimitive.Range className="ui-slider-range" />
        </SliderPrimitive.Track>
        <SliderPrimitive.Thumb
          className="ui-slider-thumb"
          aria-label={label}
          aria-valuetext={steps[at]}
        />
      </SliderPrimitive.Root>
      <div className="ui-slider-ticks" aria-hidden="true">
        {steps.map((s, i) => (
          <span key={s} className="ui-slider-tick" data-active={i === at ? "" : undefined}>
            {s}
          </span>
        ))}
      </div>
    </div>
  );
}

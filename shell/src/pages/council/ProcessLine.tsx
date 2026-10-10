import { useId } from "react";
import { Button } from "../../ui";
import { phaseLabel, processPhases, type ProcessPhase } from "./court-model";

/**
 * The line of phases under the room. In setup it shows what the council will
 * go through and carries the round controls and Convene; in a session each
 * phase that has happened is a way to read the room as it stood then.
 */
export type ProcessLineProps =
  | {
      mode: "setup";
      /** `undefined`: the config is pending, failed or names no roster. */
      rounds: number | undefined;
      maxRounds: number | undefined;
      onRounds: (rounds: number) => void;
      convene: { disabled: boolean; reason: string | null; onConvene: () => void; pending: boolean };
    }
  | {
      mode: "session";
      phases: ProcessPhase[];
      /**
       * The key of the live moment (`momentKey(liveMoment(view))`). An ended council has no `current`
       * phase, yet it still stopped somewhere: that phase is the way back to live.
       */
      liveKey: string;
      /** A `momentKey`, or `"live"`. */
      viewing: string;
      onView: (key: string | "live") => void;
    };

export function ProcessLine(props: ProcessLineProps) {
  return props.mode === "setup" ? <SetupLine {...props} /> : <SessionLine {...props} />;
}

function SetupLine({
  rounds,
  maxRounds,
  onRounds,
  convene,
}: Extract<ProcessLineProps, { mode: "setup" }>) {
  const reasonId = useId();
  const labels =
    rounds === undefined
      ? ["Opening", "Verdict"]
      : processPhases(rounds).map(phaseLabel);
  return (
    <div className="council-process">
      <ol className="council-process-phases" aria-label="Process">
        {labels.map((label, i) => (
          <li className="council-process-phase" key={`${i}-${label}`}>
            {label}
          </li>
        ))}
      </ol>
      <div className="council-process-controls">
        <Button
          variant="quiet"
          disabled={rounds === undefined || maxRounds === undefined || rounds >= maxRounds}
          onClick={() => rounds !== undefined && onRounds(rounds + 1)}
        >
          Add a round
        </Button>
        <Button
          variant="quiet"
          aria-label="Remove the last round"
          disabled={rounds === undefined || rounds <= 1}
          onClick={() => rounds !== undefined && onRounds(rounds - 1)}
        >
          ×
        </Button>
        <Button
          variant="approve"
          intent="go"
          disabled={convene.disabled}
          title={convene.reason ?? undefined}
          aria-describedby={convene.reason !== null ? reasonId : undefined}
          onClick={convene.onConvene}
        >
          <span aria-hidden="true">⚖ </span>
          {convene.pending ? "Convening…" : "Convene"}
        </Button>
        {convene.reason !== null && (
          <span className="sr-only" id={reasonId}>
            {convene.reason}
          </span>
        )}
      </div>
    </div>
  );
}

function SessionLine({ phases, liveKey, viewing, onView }: Extract<ProcessLineProps, { mode: "session" }>) {
  const viewed = viewing === "live" ? liveKey : viewing;
  return (
    <div className="council-process">
      <ol className="council-process-phases" aria-label="Process">
        {phases.map((p) => {
          const label = p.stoppedHere ? `${p.label} · stopped early` : p.label;
          return (
            <li className="council-process-phase" key={p.key} data-current={p.current || undefined}>
              {p.reachable ? (
                <button
                  type="button"
                  className="council-process-step"
                  aria-current={p.key === viewed ? "step" : undefined}
                  onClick={() => onView(p.key === liveKey ? "live" : p.key)}
                >
                  {label}
                </button>
              ) : (
                <span className={p.struck ? "council-process-phase-unreachable" : "council-process-phase-ahead"}>{label}</span>
              )}
            </li>
          );
        })}
      </ol>
    </div>
  );
}

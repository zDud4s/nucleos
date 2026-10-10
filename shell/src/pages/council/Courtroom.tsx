import { useRef, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import {
  fitSeatLabel,
  VIEW_H,
  VIEW_W,
  type Balance,
  type Court,
  type CourtJudge,
  type CourtLink,
  type CourtSeat,
  type SeatState,
} from "./court-model";

/**
 * The courtroom, drawn. Everything with a number in it comes from
 * `court-model.ts`; this file only turns that into SVG and wires the keyboard.
 * Colours and fonts live in `council.css` (`council-court-*`), never here.
 */

export type CourtSelection =
  | { kind: "seat"; index: number } // chair index in setup, seat_idx in session
  | { kind: "judge" }
  | { kind: "lectern" }
  | null;

const CX = VIEW_W / 2;

/** The scales' pivot, and the half-length of the beam, in viewBox units. */
const PIVOT = { x: 78, y: 52 };
const HALF_BEAM = 36;
const CHAIN = 14;
const PLATE = 6.5;

/** The lectern's box and its ballot tray. */
const LECTERN = { x: 200, y: 126, w: 160, h: 34 };
const TRAY = { x: 252, y: 168, w: 56, h: 12 };
const BALLOT = { w: 5.5, h: 8, gap: 1.5 };

/** The wing's text is centred here. */
const WING_X = 482;

const JUDGE_LABEL_CHARS = 16;

const clip = (text: string, room: number) => (text.length > room ? `${text.slice(0, room - 1)}…` : text);

/** "1st", "2nd", "3rd", "4th", "11th", "21st". */
export function ordinal(n: number): string {
  const teen = n % 100 >= 11 && n % 100 <= 13;
  const suffix = teen ? "th" : ({ 1: "st", 2: "nd", 3: "rd" } as Record<number, string>)[n % 10] ?? "th";
  return `${n}${suffix}`;
}

const STATE_WORDS: Partial<Record<SeatState, string>> = {
  speaking: "speaking",
  thinking: "thinking",
  ok: "done",
  cancelled: "cancelled",
  skipped: "skipped",
  waiting: "waiting",
};

/** `Seat N, name[, role][, state]` — a masked seat is named by its letter, so the label does not unmask it. */
export function seatLabel(seat: CourtSeat, mode: "setup" | "session"): string {
  const parts = [`Seat ${seat.index + 1}`, seat.state === "empty" ? "empty" : seat.label];
  if (seat.sublabel !== null) parts.push(seat.sublabel);
  if (mode === "session") {
    const word = seat.state === "failed" ? `failed: ${seat.reason ?? "failed"}` : STATE_WORDS[seat.state];
    if (word !== undefined) parts.push(word);
  }
  return parts.join(", ");
}

function seatTitle(seat: CourtSeat): string {
  if (seat.state === "empty") return `Seat ${seat.index + 1}: empty`;
  return [seat.name, seat.sublabel, seat.reason].filter((part): part is string => part !== null && part !== "").join(" · ");
}

function activate(event: KeyboardEvent, act: () => void) {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    act();
  }
}

export function Courtroom({
  court,
  mode,
  selected,
  onSelect,
  question,
  wingAction,
  settle,
}: {
  court: Court;
  mode: "setup" | "session";
  selected: CourtSelection;
  onSelect: (selection: CourtSelection) => void;
  /** Setup only: the lectern's question field, overlaid on the SVG. */
  question?: { value: string; onChange: (v: string) => void; onSubmit: () => void };
  /** Session only: the Cancel/Ask-again control, overlaid on the right wing. */
  wingAction?: ReactNode;
  /** Entry animation from setup positions. */
  settle?: boolean;
}) {
  const seatRefs = useRef<(SVGGElement | null)[]>([]);

  const onSeatKey = (event: KeyboardEvent<SVGGElement>, at: number, seat: CourtSeat) => {
    const last = court.seats.length - 1;
    let to: number | null = null;
    if (event.key === "ArrowRight" || event.key === "ArrowDown") to = Math.min(last, at + 1);
    else if (event.key === "ArrowLeft" || event.key === "ArrowUp") to = Math.max(0, at - 1);
    else if (event.key === "Home") to = 0;
    else if (event.key === "End") to = last;
    if (to !== null) {
      event.preventDefault();
      seatRefs.current[to]?.focus();
      return;
    }
    activate(event, () => onSelect({ kind: "seat", index: seat.index }));
  };

  const where = new Map<string, { x: number; y: number }>([["judge", { x: CX, y: 70 }]]);
  court.seats.forEach((seat) => where.set(seat.key, { x: seat.x, y: seat.y }));

  const lecternSelected = selected?.kind === "lectern";

  return (
    <div className={`council-court${court.dimmed ? " council-court-dimmed" : ""}`}>
      <svg
        className={`council-court-svg${settle ? " council-court-settle" : ""}`}
        viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
        role="group"
        aria-label="Courtroom"
      >
        <Pediment />
        <Judge judge={court.judge} selected={selected?.kind === "judge"} onSelect={() => onSelect({ kind: "judge" })} />
        <Scales balance={court.balance} />
        <Wing wing={court.wing} />
        {court.links.map((link, i) => (
          <Link key={`${link.kind}-${link.from}-${link.to}-${i}`} link={link} where={where} />
        ))}
        <Lectern
          court={court}
          interactive={mode === "session"}
          selected={lecternSelected}
          onSelect={() => onSelect({ kind: "lectern" })}
        />
        {court.seats.map((seat, i) => (
          <Seat
            key={seat.key}
            seat={seat}
            mode={mode}
            selected={selected?.kind === "seat" && selected.index === seat.index}
            settle={settle === true}
            setRef={(node) => {
              seatRefs.current[i] = node;
            }}
            onSelect={() => onSelect({ kind: "seat", index: seat.index })}
            onKeyDown={(event) => onSeatKey(event, i, seat)}
          />
        ))}
      </svg>
      {question !== undefined && (
        <textarea
          className="council-court-question"
          aria-label="Question"
          placeholder="Ask the council…"
          rows={1}
          value={question.value}
          onChange={(event) => question.onChange(event.target.value)}
          onFocus={() => onSelect({ kind: "lectern" })}
          onKeyDown={(event) => {
            // An IME still composing owns the Enter; it is not a send.
            if (event.nativeEvent.isComposing) return;
            if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
              event.preventDefault();
              question.onSubmit();
            }
          }}
        />
      )}
      {wingAction !== undefined && <div className="council-court-wing-action">{wingAction}</div>}
    </div>
  );
}

/* ------------------------------------------------------------------ room -- */

/** The gable and four columns around the bench, in fine line. */
function Pediment() {
  return (
    <g className="council-court-pediment" aria-hidden="true">
      <path d={`M${CX - 80} 26 L${CX} 8 L${CX + 80} 26 Z`} />
      <line x1={CX - 82} y1={30} x2={CX + 82} y2={30} />
      {[CX - 74, CX - 67, CX + 67, CX + 74].map((x) => (
        <line key={x} x1={x} y1={32} x2={x} y2={78} />
      ))}
      <line x1={CX - 82} y1={80} x2={CX + 82} y2={80} />
    </g>
  );
}

function Judge({ judge, selected, onSelect }: { judge: CourtJudge; selected: boolean; onSelect: () => void }) {
  const name = judge.state === "empty" ? "empty" : judge.name;
  return (
    <g
      role="button"
      tabIndex={0}
      className="council-court-judge"
      data-state={judge.state}
      aria-pressed={selected}
      aria-label={`Chairman, ${name}`}
      onClick={onSelect}
      onKeyDown={(event) => activate(event, onSelect)}
    >
      <title>{judge.sub === null ? `Chairman: ${name}` : `Chairman: ${name} · ${judge.sub}`}</title>
      <rect className="council-court-bench" x={CX - 60} y={40} width={120} height={30} rx={5} />
      <text className="council-court-judge-label" x={CX} y={judge.sub === null ? 59 : 54} textAnchor="middle">
        {clip(judge.label, JUDGE_LABEL_CHARS)}
      </text>
      {judge.sub !== null && (
        <text className="council-court-small" x={CX} y={65} textAnchor="middle">
          {clip(judge.sub, 22)}
        </text>
      )}
    </g>
  );
}

/**
 * The scales. The beam (with its chains) rotates as one; each plate rotates
 * back the same angle around its own hook, so it stays level.
 */
function Scales({ balance }: { balance: Balance }) {
  const tilt = (deg: number, x: number, y: number): CSSProperties => ({
    transform: `rotate(${deg}deg)`,
    transformOrigin: `${x}px ${y}px`,
  });
  const left = PIVOT.x - HALF_BEAM;
  const right = PIVOT.x + HALF_BEAM;
  const plate = (x: number) => (
    <g className="council-court-plate" style={tilt(balance.angle, x, PIVOT.y)}>
      <line x1={x} y1={PIVOT.y} x2={x - PLATE} y2={PIVOT.y + CHAIN} />
      <line x1={x} y1={PIVOT.y} x2={x + PLATE} y2={PIVOT.y + CHAIN} />
      <path
        className="council-court-plate-dish"
        d={`M${x - PLATE - 2} ${PIVOT.y + CHAIN} Q${x} ${PIVOT.y + CHAIN + 9} ${x + PLATE + 2} ${PIVOT.y + CHAIN} Z`}
      />
    </g>
  );
  return (
    <g className="council-court-scales" data-tone={balance.tone}>
      <line x1={PIVOT.x} y1={PIVOT.y - 6} x2={PIVOT.x} y2={PIVOT.y + 34} />
      <path
        className="council-court-scales-base"
        d={`M${PIVOT.x - 14} ${PIVOT.y + 36} Q${PIVOT.x} ${PIVOT.y + 28} ${PIVOT.x + 14} ${PIVOT.y + 36} Z`}
      />
      <g className="council-court-beam" style={tilt(-balance.angle, PIVOT.x, PIVOT.y)}>
        <line x1={left} y1={PIVOT.y} x2={right} y2={PIVOT.y} />
        {plate(left)}
        {plate(right)}
      </g>
      <circle className="council-court-pivot" cx={PIVOT.x} cy={PIVOT.y} r={3} />
      <text className="council-court-balance-label" x={PIVOT.x} y={108} textAnchor="middle">
        {balance.label}
      </text>
      {balance.sub !== null && (
        <text className="council-court-small" x={PIVOT.x} y={120} textAnchor="middle">
          {balance.sub}
        </text>
      )}
    </g>
  );
}

function Wing({ wing }: { wing: Court["wing"] }) {
  const title = wing.title.split(" · ");
  const sub = wing.sub === null ? [] : wing.sub.split(" · ");
  return (
    <g className="council-court-wing">
      {title.map((line, i) => (
        <text key={`t${i}`} className="council-court-wing-title" x={WING_X} y={34 + i * 14} textAnchor="middle">
          {line}
        </text>
      ))}
      {sub.map((line, i) => (
        <text
          key={`s${i}`}
          className="council-court-small"
          x={WING_X}
          y={34 + title.length * 14 + 4 + i * 12}
          textAnchor="middle"
        >
          {line}
        </text>
      ))}
    </g>
  );
}

function Link({ link, where }: { link: CourtLink; where: Map<string, { x: number; y: number }> }) {
  const a = where.get(link.from);
  const b = where.get(link.to);
  if (a === undefined || b === undefined) return null;
  return (
    <line
      className={`council-court-link council-court-link-${link.kind}`}
      x1={a.x}
      y1={a.y}
      x2={b.x}
      y2={b.y}
    >
      <title>{link.title}</title>
    </line>
  );
}

function Lectern({
  court,
  interactive,
  selected,
  onSelect,
}: {
  court: Court;
  interactive: boolean;
  selected: boolean;
  onSelect: () => void;
}) {
  const { ballots } = court;
  const box = (
    <>
      <rect className="council-court-lectern" x={LECTERN.x} y={LECTERN.y} width={LECTERN.w} height={LECTERN.h} rx={5} />
      <text className="council-court-small" x={CX} y={LECTERN.y + 11} textAnchor="middle">
        THE CASE
      </text>
      {/* In setup the question is the textarea laid over this box; in a session it is read here. */}
      {interactive && (
        <text className="council-court-lectern-text" x={CX} y={LECTERN.y + 26} textAnchor="middle">
          {court.lectern.text}
        </text>
      )}
    </>
  );
  const w = BALLOT.w;
  const x0 = ballots === null ? 0 : CX - (ballots.total * w + (ballots.total - 1) * BALLOT.gap) / 2;
  const slot = (i: number) => x0 + i * (w + BALLOT.gap);
  return (
    <>
      {interactive ? (
        <g
          role="button"
          tabIndex={0}
          className="council-court-lectern-group"
          aria-pressed={selected}
          aria-label="The case"
          onClick={onSelect}
          onKeyDown={(event) => activate(event, onSelect)}
        >
          <title>{court.lectern.full}</title>
          {box}
        </g>
      ) : (
        <g className="council-court-lectern-group" data-selected={selected || undefined}>
          {box}
        </g>
      )}
      {ballots !== null && (
        <g className="council-court-tray">
          <rect x={TRAY.x} y={TRAY.y} width={TRAY.w} height={TRAY.h} rx={2} />
          {Array.from({ length: ballots.total }, (_, i) =>
            i < ballots.got ? (
              <rect
                key={`cast-${ballots.cast[i]?.seat ?? i}`}
                className="council-court-ballot"
                x={slot(i)}
                y={TRAY.y + 2}
                width={w}
                height={BALLOT.h}
                rx={1}
                style={
                  {
                    "--from-x": `${(ballots.cast[i]?.from.x ?? slot(i)) - slot(i)}px`,
                    "--from-y": `${(ballots.cast[i]?.from.y ?? TRAY.y) - (TRAY.y + 2)}px`,
                  } as CSSProperties
                }
              />
            ) : (
              <rect
                key={`slot-${i}`}
                className="council-court-ballot-slot"
                x={slot(i)}
                y={TRAY.y + 2}
                width={w}
                height={BALLOT.h}
                rx={1}
              />
            ),
          )}
          <text className="council-court-small" x={CX} y={TRAY.y + TRAY.h + 12} textAnchor="middle">
            {`ballots ${ballots.got}/${ballots.total}`}
          </text>
        </g>
      )}
    </>
  );
}

/* ------------------------------------------------------------------ seat -- */

function Seat({
  seat,
  mode,
  selected,
  settle,
  setRef,
  onSelect,
  onKeyDown,
}: {
  seat: CourtSeat;
  mode: "setup" | "session";
  selected: boolean;
  settle: boolean;
  setRef: (node: SVGGElement | null) => void;
  onSelect: () => void;
  onKeyDown: (event: KeyboardEvent<SVGGElement>) => void;
}) {
  const { x, y, r } = seat;
  const style = settle
    ? ({ "--from-x": `${seat.from.x - x}px`, "--from-y": `${seat.from.y - y}px` } as CSSProperties)
    : undefined;
  // Inside the circle: the short name, sized to the disc. The full one stays on the accessible name and title.
  const fit = fitSeatLabel(seat.label, r);
  const reveal = fitSeatLabel(seat.name, r);
  const tagY = y + r + (seat.sublabel === null ? 10 : 20);
  return (
    <g
      ref={setRef}
      role="button"
      tabIndex={0}
      className="council-court-seat"
      data-state={seat.state}
      data-masked={seat.masked || undefined}
      aria-pressed={selected}
      aria-label={seatLabel(seat, mode)}
      style={style}
      onClick={onSelect}
      onKeyDown={onKeyDown}
    >
      <title>{seatTitle(seat)}</title>
      <circle className="council-court-seat-disc" cx={x} cy={y} r={r} />
      {seat.masked ? (
        <>
          <text className="council-court-letter" x={x} y={y + 4} textAnchor="middle">
            {seat.label}
          </text>
          <text className="council-court-reveal" x={x} y={y + 3} textAnchor="middle" style={{ fontSize: reveal.fontSize }}>
            {reveal.text}
          </text>
        </>
      ) : (
        <text className="council-court-seat-name" x={x} y={y + 3.5} textAnchor="middle" style={{ fontSize: fit.fontSize }}>
          {fit.text}
        </text>
      )}
      {seat.state === "ok" && (
        <text className="council-court-tick" x={x + r * 0.72} y={y - r * 0.55} textAnchor="middle">
          ✓
        </text>
      )}
      {seat.sublabel !== null && (
        <text className="council-court-role" x={x} y={y + r + 10} textAnchor="middle">
          {clip(seat.sublabel, 16)}
        </text>
      )}
      {(seat.rank !== null || seat.dissent) && (
        <text className="council-court-rank" x={x} y={tagY} textAnchor="middle">
          {seat.rank !== null && <tspan>{ordinal(seat.rank)}</tspan>}
          {seat.dissent && (
            <tspan className="council-court-dissent">{seat.rank !== null ? " · dissent" : "dissent"}</tspan>
          )}
        </text>
      )}
      {(seat.state === "thinking" || seat.state === "speaking") && (
        <text className="council-court-small council-court-seat-state" x={x} y={y + r + (seat.sublabel === null ? 10 : 20)} textAnchor="middle">
          {seat.state === "thinking" ? "thinking…" : "speaking"}
        </text>
      )}
    </g>
  );
}

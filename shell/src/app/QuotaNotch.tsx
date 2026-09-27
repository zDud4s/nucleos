import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { Pin, PinOff } from "lucide-react";
import { useQuota, type QuotaProvider, type QuotaWindow } from "../data/quota";
import { IconButton, ProviderMark, Ring, readState, relativeText, type RingTrack } from "../ui";
import type { NotchMode } from "./notch-mode";
import { MIDDLE } from "./notch-place";

/**
 * The two windows every provider is drawn with, outermost first.
 *
 * Fixed here rather than taken from the answer, and that is the point: a provider whose quota could
 * not be read carries no windows at all, and a notch built from whatever arrived would silently
 * lose its rings instead of drawing them dashed. The names are the daemon's vocabulary (`5h`,
 * `7d`), not the vendor's.
 */
const WINDOWS = ["7d", "5h"] as const;

/**
 * How long the panel stays open after the pointer leaves it.
 *
 * **Folding on the instant was the other half of the bumpy hover.** A pointer that slips a pixel
 * past the rounded corner, or across the edge while the floating window is still being resized
 * round the panel, reads as a leave — and a notch that folds on every leave and unfolds on every
 * enter flickers between the two for as long as somebody is trying to read it. A quarter of a
 * second is long enough to forgive that and short enough that a notch the pointer has really left
 * is already going by the time anybody looks back. A pointer that returns in the meantime cancels
 * it, so the panel never folds under somebody reading it.
 */
const LINGER_MS = 240;

/** How long the panel takes to go: `--dur-quick`, the length of `quota-notch-fold` (`app.css`). */
const FOLD_MS = 140;

/**
 * Where the bubble's tail sits when nothing pushes it: level with the middle of the title row.
 *
 * The bubble is placed so the tail lands on the middle of the ring it is about, and this is the
 * distance from the bubble's top edge to that tail. A ring too near the top of the notch to keep it
 * — the first one always is — moves the tail up instead of the bubble, so the bubble never starts
 * above the drawing, where the floating window has no room for it.
 */
const TAIL_AT = 26;

/**
 * How far a press has to move, in screen pixels, before it is a drag rather than a click. Four is
 * past the jitter of a hand settling on a touchpad and short of anything anybody would call a move.
 */
const DRAG_SLOP = 4;

/**
 * A drag of the rail: the screen height it started at, the fraction it started from, and whether
 * it has moved past `DRAG_SLOP` yet.
 */
interface Drag {
  from: number;
  start: number;
  moved: boolean;
}

/**
 * The height a drag is measured against: the screen's work area, in CSS pixels — the same area the
 * fraction is a fraction of. The window's own height where the screen reports none (jsdom does).
 */
function screenSpan(): number {
  const available = window.screen?.availHeight ?? 0;
  return available > 0 ? available : Math.max(1, window.innerHeight);
}

/**
 * Folded and open are the two states; `folding` is the moment between them, when the panel is
 * still drawn and playing its way out. The drawing only shrinks once that is done — which in the
 * floating host is also when its window shrinks — so what the owner sees go is the panel, never a
 * rectangle of text cut off by a window closing round it.
 */
type Phase = "folded" | "open" | "folding";

export interface QuotaNotchProps {
  /**
   * Which host is drawing it (design D8). `contained` is the right edge of the page area, `global`
   * a window of its own against the right edge of the screen. Both are folded at rest and unfold
   * when the pointer reaches them, or when focus does. Focus is the weaker of the two in the
   * floating host, and deliberately said so: that window is outside the Alt+Tab order, so focus
   * only ever arrives after a click or from assistive tech (see the move control below).
   */
  host?: NotchMode;
  /**
   * Moves the notch to the other host. Absent, no control is drawn — a test or a preview that has
   * no second host to offer does not offer one.
   */
  onMove?: () => void;
  /**
   * The line of the screen the floating notch is centred on, in this window's CSS pixels
   * (`useScreenLine`). The contained host hangs from it so docking and floating do not move the
   * notch; absent, it centres in the window. A prop rather than measured here, because this file is
   * also the floating window's page, and that window may not call the window API at all.
   */
  line?: number;
  /**
   * How far down the edge the notch hangs, a fraction of the screen's work area (`notch-place.ts`).
   * Used here only for the fallback when there is no `line` to hang from; the host that owns it is
   * what turns it into a place.
   */
  along?: number;
  /**
   * Moves the notch along the edge: `done` false for every step of a drag, true for the drop.
   * Absent, the rail cannot be dragged — a test or a preview with nowhere to keep a position.
   */
  onAlong?: (along: number, done: boolean) => void;
}

/**
 * How much of each assistant's usage window is gone, hanging off an edge.
 *
 * **A column against the right edge, and not a bar along the top.** The edge decides the axis:
 * left and right keep a vertical column, top and bottom would lay the readings out side by side.
 * The right edge is what this draws, because it is the edge with the most room to grow into — the
 * app's own page is centred with slack on both sides, and a screen has more width to spare than
 * height. The choice becomes a setting with the rest of the policy (design D9's phase), and the
 * shape here is what that setting will switch between rather than something it has to undo.
 *
 * **Folded at rest, in both hosts, and that changed.** The contained host used to be drawn open
 * always, on the argument that a page has room to spare. A lateral column makes that argument
 * false: open always, it is a wall down the right-hand side of whatever page is in front. So both
 * hosts now show the rings alone until somebody asks, and the asking is a hover or a focus.
 *
 * **The panel opens from where the rings are, and it waits before it goes.** Both hosts hang the
 * notch by the top of its FOLDED drawing — `--quota-notch-rest` here, `rest` in `notch_fit` for
 * the floating window — so unfolding grows the drawing down and to the left, and the first ring
 * stays exactly where the pointer found it. Centred on its own height, as it was, every unfold
 * moved the rings away by half of what it grew. And leaving folds only after `LINGER_MS`, so a
 * pointer that slips off an edge for a moment is not answered with a fold and a re-open.
 *
 * **One component, two hosts.** Nothing about the reading changes between them — only which way
 * the move control points. The contained host is also the recoil if the floating window misbehaves
 * (risk R1), which is why it keeps working with no Rust side at all.
 *
 * **A folded notch must not draw an old answer like a live one.** `GET /quota` answers 200 with
 * `source: "stored"` when the quota sidecar cannot be reached, carrying whatever figures were
 * last recorded — so an outage arrives as numbers rather than as an error. Unfolded, the notch
 * says "last known" in words. Folded, which is where it spends its life, it said nothing whatever,
 * and that is the one thing this app forbids itself: stale and current may never render as one. So
 * the whole drawing carries `data-stored`, and the rail's readings go quiet (`app.css`). It was a
 * dashed edge once, and the owner did not want a frame flickering round the notch every time the
 * sidecar missed a refresh. No tone either way, because `.ui-note-stale` settled the house style:
 * say stale without borrowing a colour that would claim something about the quota instead of
 * about the reading of it.
 *
 * **The rail never changes shape; the reading opens beside it.** The unfolded notch used to grow
 * the rail itself into a wide panel holding every provider at once. It now opens a bubble to the
 * left of the rail for ONE provider — the ring under the pointer — with a tail pointing at that
 * ring, which is the owner's reference for this design. The rail keeps its width, its flared ends
 * and its rings exactly where they were, so nothing the pointer is on moves.
 *
 * **A mark on the rail, a name in the bubble.** On the rail the provider is a glyph inside its ring:
 * at that size the word cost more room than the drawing it labelled. The bubble has the room, and
 * titles itself with the mark and the name — the first thing a reader of it wants to know, and the
 * one thing the ring beside it could only imply. Colour reinforces and never states.
 *
 * **One figure at rest, and that is a reversal.** The folded notch used to be arcs alone — the
 * numbers waited for the pointer — and the owner's reference for this design puts a percentage
 * under every ring, which is what the rest of the drawing was already implying and refusing to
 * say. An arc answers *roughly how much*; the number beside it is what somebody decides on, and
 * making them hover for it costs a gesture to read the one thing the notch exists for. So each
 * ring now carries the fullest of its windows, WITH that window's name — a bare percentage over
 * two windows would be a figure nobody can attribute. Everything else still waits for the pointer:
 * the second window, the resets, the fidelity and the way to the other host.
 */
export function QuotaNotch({ host = "contained", onMove, line, along, onAlong }: QuotaNotchProps) {
  const quota = useQuota();
  const [phase, setPhase] = useState<Phase>("folded");
  // Which provider the bubble is about: the ring the pointer last reached, or the first one when
  // the notch was opened some other way (focus, or the pointer arriving on the rail between rings).
  const [active, setActive] = useState(0);
  const timer = useRef<number | undefined>(undefined);
  const drawing = useRef<HTMLDivElement>(null);
  const rail = useRef<HTMLDivElement>(null);
  // The drawing's height folded, for the contained host to hang from (`app.css`,
  // `.quota-notch-contained`). Measured rather than derived: it is two rings or three, the flares
  // and some padding, and a sum of tokens written out here would be one more thing to keep equal.
  const [rest, setRest] = useState<number | undefined>(undefined);
  // Where the bubble sits and where its tail points, both measured from the top of the drawing.
  const [aim, setAim] = useState({ top: 0, tail: TAIL_AT });
  const count = quota.data?.providers.length ?? 0;
  const reached = phase !== "folded";
  // A drag of the rail under way: where it started, and whether it has moved far enough to be one.
  const drag = useRef<Drag | null>(null);
  const [dragging, setDragging] = useState(false);
  // The latest position of a drag not yet handed over, coalesced to one per frame.
  const pending = useRef<{ frame: number; along: number } | null>(null);

  useEffect(
    () => () => {
      window.clearTimeout(timer.current);
      if (pending.current !== null) window.cancelAnimationFrame(pending.current.frame);
    },
    [],
  );

  // Before paint, so the first frame is already hung from the right place.
  useLayoutEffect(() => {
    if (phase !== "folded" || drawing.current === null) return;
    setRest(drawing.current.getBoundingClientRect().height);
  }, [phase, count]);

  // The tail points at the middle of the ring the bubble is about. Measured, because the ring's
  // place in the rail depends on how many providers sit above it; and the bubble moves down only as
  // far as it has to, so the first ring's bubble starts level with the top of the notch instead of
  // above it, where the floating window has no room.
  useLayoutEffect(() => {
    if (!reached || drawing.current === null || rail.current === null) return;
    const ring = rail.current.querySelectorAll(".ui-ring")[active];
    if (ring === undefined) return;
    const face = ring.getBoundingClientRect();
    const centre = face.top + face.height / 2 - drawing.current.getBoundingClientRect().top;
    const top = Math.max(0, centre - TAIL_AT);
    setAim((current) =>
      current.top === top && current.tail === centre - top ? current : { top, tail: centre - top },
    );
  }, [reached, active, count]);

  // Nothing until the first answer. A notch drawn empty would say "nothing is burned", which is the
  // most misleading thing this feature could claim — and it would say it at exactly the moment
  // nobody has measured anything yet.
  if (quota.data === undefined || quota.data.providers.length === 0) return null;

  const { providers, source, unreachable } = quota.data;
  // One instant for the whole drawing. Read once rather than per line, so two windows in the same
  // notch cannot be counted against two different nows — a difference of milliseconds that shows
  // up as "resets in 1h" beside "resets in 59min".
  const now = Date.now();
  const shown = providers[Math.min(active, providers.length - 1)];
  const shownTracks = tracksOf(shown);

  const unfold = () => {
    // Nothing opens under a drag: the pointer is on the notch because it is carrying it.
    if (drag.current?.moved) return;
    window.clearTimeout(timer.current);
    setPhase("open");
  };

  // Every step of a drag goes to the host once per frame at most. The floating host answers each
  // with a fit, which moves a window — sixty of those a second is the ceiling worth paying, and a
  // pointer reports far more often than that on a fast mouse.
  const carry = (next: number) => {
    if (onAlong === undefined) return;
    if (pending.current !== null) {
      pending.current.along = next;
      return;
    }
    const frame = window.requestAnimationFrame(() => {
      const last = pending.current;
      pending.current = null;
      if (last !== null) onAlong(last.along, false);
    });
    pending.current = { frame, along: next };
  };

  const drop = (next: number) => {
    if (pending.current !== null) {
      window.cancelAnimationFrame(pending.current.frame);
      pending.current = null;
    }
    onAlong?.(next, true);
  };
  // Every step checks where it is before it moves, so a leave that arrives with the notch already
  // folded plays nothing, and a pointer back inside before the linger is up folds nothing.
  const fold = (linger: number) => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      setPhase((current) => (current === "open" ? "folding" : current));
      timer.current = window.setTimeout(
        () => setPhase((current) => (current === "folding" ? "folded" : current)),
        FOLD_MS,
      );
    }, linger);
  };

  return (
    <div
      ref={drawing}
      className={`quota-notch quota-notch-${host}`}
      data-unfolded={reached}
      data-folding={phase === "folding"}
      data-stored={source === "stored"}
      data-anchored={line !== undefined}
      style={
        rest === undefined
          ? undefined
          : ({
              "--quota-notch-rest": `${rest}px`,
              "--quota-notch-along": `${along ?? MIDDLE}`,
              ...(line === undefined ? {} : { "--quota-notch-line": `${line}px` }),
            } as CSSProperties)
      }
      onPointerEnter={unfold}
      onPointerLeave={() => fold(LINGER_MS)}
      onFocus={unfold}
      onBlur={(event) => {
        // Focus has nothing to slip off, so it lingers for nothing — but the bubble still plays out.
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) fold(0);
      }}
    >
      {/*
        The bubble: one provider's reading, beside the rail and pointing at its ring.

        Always in the tree, and `.sr-only` while folded, for the move control's sake — see the
        comment on it below. Folded it holds nothing else, so nothing of the reading is on the page,
        drawn or not, until somebody asks.
      */}
      <div
        className={reached ? "quota-notch-pop" : "sr-only"}
        style={
          reached
            ? ({
                "--quota-notch-pop-top": `${aim.top}px`,
                "--quota-notch-tail-top": `${aim.tail}px`,
              } as CSSProperties)
            : undefined
        }
      >
        {reached && (
          <div className="quota-notch-detail">
            {/*
              Whose reading this is, in words as well as in the mark. The bubble is the one place
              with room for the name, and it is the first thing a reader of it wants.
            */}
            <span className="quota-notch-title">
              <ProviderMark provider={shown.provider} size={16} />
              <span className="quota-notch-name">{`${displayName(shown.provider)} usage`}</span>
              {source === "stored" && (
                /*
                  The last known figures, with the sidecar unreachable. Said rather than implied:
                  these numbers are real and they are old, and a reader who takes them for live ones
                  is reading a quota that may have moved a long way since.
                */
                <span className="quota-notch-stale" title={unreachable}>
                  last known
                </span>
              )}
            </span>
            {shownTracks.map((track) => (
              <span className="quota-notch-line" key={track.name}>
                <span className="quota-notch-head">
                  <span className="quota-notch-window">{windowTitle(track.name)}</span>
                  <span className="quota-notch-reset">{resetPhrase(shown, track.name, now)}</span>
                </span>
                {/*
                  The same fraction the ring's arc draws, laid flat: a length is what the eye
                  compares without arithmetic. `aria-hidden`, because the percentage under it is the
                  same fact in words and the ring already carries the sentence.
                */}
                <span
                  className={track.measured ? "quota-notch-bar" : "quota-notch-bar quota-notch-bar-absent"}
                  aria-hidden="true"
                >
                  {track.measured && (
                    <span
                      className={`quota-notch-bar-fill quota-notch-bar-${toneOf(track)}`}
                      style={{ width: `${Math.min(100, Math.max(0, track.used * 100))}%` }}
                    />
                  )}
                </span>
                <span className="quota-notch-figure">
                  {/*
                    The dash takes a class of its own because it is not a figure. In the weight and
                    the ink the percentages wear, an em dash reads as a value somebody measured; what
                    it means is that nobody could.
                  */}
                  <span
                    className={
                      track.measured ? "quota-notch-percent" : "quota-notch-percent quota-notch-percent-absent"
                    }
                  >
                    {track.measured ? `${Math.round(track.used * 100)}%` : "—"}
                  </span>
                  {/* Only beside a figure: "— used" would say something about a window nobody read. */}
                  {track.measured && <span className="quota-notch-used">used</span>}
                </span>
              </span>
            ))}
          </div>
        )}
        <span className="quota-notch-foot">
          {/*
            How the figure was come by, and how old it is — the two facts that decide what the
            percentages above are worth. `derived` is named rather than hidden because it means
            something the owner has to weigh: that reading only moves when they run something, so an
            hour-old one is normal and an hour-old `official` one is not.
          */}
          {reached && <span className="quota-notch-fidelity">{fidelityPhrase(shown, now)}</span>}
          {onMove !== undefined && (
            /*
              Folded, the control is still THERE, inside the `.sr-only` bubble rather than left
              unrendered. Rendered only when unfolded it could not be reached by a keyboard at all:
              the wrapper's `onFocus` fires from a child, and the only focusable child is this
              button, so focus would have nowhere to land and the notch would never unfold. Hidden
              that way it is out of flow — it measures nothing, so neither the page nor the window
              the Rust side fits round this drawing changes size — and it is a tab stop, which
              unfolds the notch and brings itself into view. It stays the same element across the
              unfold, because nothing above it in the tree changes shape.

              What that does NOT buy in the floating host: reaching its window from the keyboard in
              the first place. It is built `skip_taskbar(true)` (`notch.rs`), which on Windows means
              WS_EX_TOOLWINDOW and no place in the Alt+Tab order, and `focused(false)`, so it never
              takes focus by itself. Focus arrives when the owner clicks the notch, or through
              assistive tech that can move it. So an owner working from the keyboard alone cannot
              reach the way back at all, and the main window has no other: `AppShell` draws no notch
              while the mode is `global`, by design, and the mode has no home in settings yet. That
              gap is named here rather than implied away.
            */
            <span className={reached ? "quota-notch-control" : undefined}>
              {host === "contained" ? (
                <IconButton label="Keep the notch in front of every window" icon={Pin} onClick={onMove} />
              ) : (
                <IconButton label="Put the notch back inside NucleOS" icon={PinOff} onClick={onMove} />
              )}
            </span>
          )}
        </span>
      </div>
      {/*
        The rail: the part of the notch that is always there, and never changes width. The bubble
        opens beside it rather than inside it, so reaching a ring never moves a ring.

        The two flares are the ends of the notch, where it curves out to meet the edge it hangs
        from — the shape that says this is attached to the edge of the screen rather than parked
        near it. Elements rather than pseudo-elements, so they are in the flow and the floating
        window, fitted to the drawing's measured box, has room for them.
      */}
      {/*
        The rail is also the handle: pressed and moved up or down, it carries the notch along the
        edge (`notch-place.ts` keeps where it was left). A press that moves less than `DRAG_SLOP`
        is not a drag, so a click on a ring stays a click. Measured in SCREEN pixels against the
        screen's work area, because in the floating host the window moves under the pointer on every
        step — `clientY` would be measured from a window that is itself on the move — and because the
        fraction it produces is a fraction of that same work area. Pointer capture keeps the moves
        coming when the pointer outruns the notch. A double click puts it back in the middle.
      */}
      <div
        className="quota-notch-rail"
        ref={rail}
        data-draggable={onAlong !== undefined && along !== undefined}
        data-dragging={dragging}
        onPointerDown={(event) => {
          if (onAlong === undefined || along === undefined || event.button !== 0) return;
          drag.current = { from: event.screenY, start: along, moved: false };
          event.currentTarget.setPointerCapture?.(event.pointerId);
        }}
        onPointerMove={(event) => {
          const current = drag.current;
          if (current === null) return;
          const moved = event.screenY - current.from;
          if (!current.moved) {
            if (Math.abs(moved) < DRAG_SLOP) return;
            current.moved = true;
            setDragging(true);
            // The bubble goes at once rather than playing out: it points at a ring that is leaving.
            window.clearTimeout(timer.current);
            setPhase("folded");
          }
          carry(current.start + moved / screenSpan());
        }}
        onPointerUp={(event) => {
          const current = drag.current;
          drag.current = null;
          if (event.currentTarget.hasPointerCapture?.(event.pointerId)) {
            event.currentTarget.releasePointerCapture(event.pointerId);
          }
          if (current === null || !current.moved) return;
          setDragging(false);
          drop(current.start + (event.screenY - current.from) / screenSpan());
        }}
        onPointerCancel={() => {
          const current = drag.current;
          drag.current = null;
          if (current === null || !current.moved) return;
          // Cancelled by the system mid-drag: the notch stays where it was carried to, and that is
          // what is remembered — the last position it was actually drawn at.
          setDragging(false);
          if (pending.current !== null) drop(pending.current.along);
          else if (along !== undefined) drop(along);
        }}
        onDoubleClick={() => onAlong?.(MIDDLE, true)}
      >
        <span className="quota-notch-flare quota-notch-flare-top" aria-hidden="true" />
        <div className="quota-notch-body">
          {providers.map((provider, index) => {
            // One reading of the windows for the slot: the ring and the figure under it are two
            // drawings of the same tracks, and building them twice is how they start disagreeing.
            const tracks = tracksOf(provider);
            const worst = fullest(tracks);
            return (
              <div
                className="quota-notch-slot"
                key={provider.provider}
                title={titleOf(provider)}
                onPointerEnter={() => setActive(index)}
              >
                <Ring
                  label={provider.provider}
                  tracks={tracks}
                  mark={<ProviderMark provider={provider.provider} />}
                />
                {/*
                  The one figure the notch says at rest: the window with least left in it, named. A
                  provider has two windows, and a bare percentage would be a number nobody can
                  attribute — 56% of five hours and 56% of seven days mean entirely different things
                  about what is left to spend. The fullest rather than the shorter, because the
                  constraint that binds first is what a glance needs.
                */}
                <span className="quota-notch-headline">
                  {worst === undefined ? (
                    <span className="quota-notch-headline-percent quota-notch-headline-absent">—</span>
                  ) : (
                    <>
                      <span className="quota-notch-headline-percent">{`${Math.round(worst.used * 100)}%`}</span>
                      <span className="quota-notch-headline-window">{worst.name}</span>
                    </>
                  )}
                </span>
              </div>
            );
          })}
        </div>
        <span className="quota-notch-flare quota-notch-flare-bottom" aria-hidden="true" />
      </div>
    </div>
  );
}

/** The provider's two rings: the window if it was read, a dashed track if it was not. */
function tracksOf(provider: QuotaProvider): RingTrack[] {
  return WINDOWS.map((name) => {
    const window = provider.windows.find((candidate) => candidate.window === name);
    if (window === undefined) {
      return { domain: "quota", state: "unmeasured", name, used: 0, measured: false };
    }
    return {
      domain: "quota",
      state: window.state,
      name,
      used: window.used_fraction,
      // A stale figure is drawn as a figure — it is real, about a window that has since rolled
      // over — and its tone is what says so. Only an absent reading is dashed.
      measured: true,
    };
  });
}

/**
 * The window with least left in it, or nothing when none of them is a live figure.
 *
 * **A stale window is not a candidate, and that is the whole care in this function.** `stale` means
 * the figure is real and the window it describes has already rolled over — so the number is almost
 * certainly no longer true, and what is left of that window is probably all of it. The panel can
 * print such a figure because it prints "reset 2h ago" beside it; the rest figure has one line and
 * no room to say so, and 97% under a ring whose arc is grey would be the app drawing stale and
 * current as one thing (`PRODUCT.md`). A provider with nothing but stale windows says the same dash
 * as one nobody could read at all: no current figure. The arcs still carry the reading, and the
 * hover text still explains it.
 *
 * Ties go to the outermost track, which is the order `WINDOWS` is written in — two windows at the
 * same percentage are the same news, and the long one is the one that stays true longer.
 */
function fullest(tracks: RingTrack[]): RingTrack | undefined {
  return tracks
    .filter((track) => track.measured && track.state !== "stale")
    .reduce<RingTrack | undefined>(
      (worst, track) => (worst === undefined || track.used > worst.used ? track : worst),
      undefined,
    );
}

/**
 * The bar's tone, from the one map — the same reading `Ring` paints its arc with.
 *
 * Never chosen here, for the reason the ring gives: a bar and an arc drawn from the same track may
 * not disagree about what it means, and `readState` is what makes that structural rather than
 * remembered. A state the map has no reading for falls to `off`, the tone with the least claim in
 * it.
 */
function toneOf(track: RingTrack): string {
  return readState(track.domain, track.state)?.tone ?? "off";
}

/**
 * The provider's name as a title: the daemon's own word, capitalised. `claude` becomes `Claude`
 * and an unknown provider still gets a name, which is what the fallback mark does for its glyph.
 */
function displayName(provider: string): string {
  const name = provider.trim();
  return name === "" ? "Unknown" : name[0].toUpperCase() + name.slice(1);
}

/**
 * A window's name in words, for the bubble's rows. The ring and the rest figure keep the daemon's
 * short form (`5h`), where room is what runs out; the bubble has the room, and "5h" as the heading
 * of a row reads as a duration somebody measured rather than as the name of a window.
 */
function windowTitle(name: string): string {
  if (name === "5h") return "5-hour window";
  if (name === "7d") return "7-day window";
  return name;
}

/**
 * When a window reopens, in the words this app already uses for a time.
 *
 * Through `relativeText` rather than a second formatter, and that is worth naming because the
 * reference design this came from writes two units — "Resets in 3 Days 3h". One spelling of a
 * duration beats fidelity to it: a second one drifts, and this app has said "in 3d" everywhere
 * else since long before the notch existed.
 *
 * The verb carries the sign, because a reset in the past is not a countdown and must not read like
 * one. A window with no announced reset says so: that is a real answer from a provider, not a
 * missing field.
 */
function resetPhrase(provider: QuotaProvider, name: string, now: number): string {
  const window = provider.windows.find((candidate) => candidate.window === name);
  if (window === undefined) return "not read";
  if (window.resets_at === null) return "no reset announced";
  const when = Date.parse(window.resets_at);
  if (Number.isNaN(when)) return window.resets_at;
  return `${when > now ? "resets " : "reset "}${relativeText(when, now)}`;
}

/** How the figure was come by, and how old it is. An unmeasured provider says why instead. */
function fidelityPhrase(provider: QuotaProvider, now: number): string {
  if (provider.fidelity === "unmeasured") {
    return provider.detail === "" ? "not read" : provider.detail;
  }
  const read = Date.parse(provider.read_at);
  if (Number.isNaN(read)) return provider.fidelity;
  return `${provider.fidelity}, ${relativeText(read, now)}`;
}

/**
 * The hover text: the fidelity, the age, and each window in words.
 *
 * It carries the provider's NAME, which the drawing no longer prints. That is the trade the mark
 * makes: a glyph is recognised faster than a word is read, and the word is one hover away for
 * anybody who does not know the glyph yet.
 */
function titleOf(provider: QuotaProvider): string {
  // The vendor's own word for how bad this is, when it sent one. `QuotaProvider.severity` is
  // documented as shown and never acted on, and it was neither: the field arrived, nothing drew
  // it, and the doc described a plan. Here is where it belongs — beside the fidelity, which is
  // the other fact about the READING rather than about one window of it.
  const said = provider.severity.trim() === "" ? "" : ` · ${provider.severity.trim()}`;
  const head = `${provider.provider} — ${provider.fidelity}${said}`;
  if (provider.windows.length === 0) {
    return provider.detail === "" ? head : `${head}: ${provider.detail}`;
  }
  return `${head}\n${provider.windows.map(describe).join("\n")}`;
}

function describe(window: QuotaWindow): string {
  const used = `${window.window} ${Math.round(window.used_fraction * 100)}%`;
  if (window.stale) return `${used} (this window has since reset)`;
  if (window.resets_at === null) return used;
  return `${used}, resets ${new Date(window.resets_at).toLocaleString()}`;
}

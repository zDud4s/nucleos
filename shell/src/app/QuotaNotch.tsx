import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { Pin, PinOff } from "lucide-react";
import { useQuota, type QuotaProvider, type QuotaWindow } from "../data/quota";
import { IconButton, ProviderMark, Ring, relativeText, type RingTrack } from "../ui";
import type { NotchMode } from "./notch-mode";

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
 * the whole drawing carries `data-stored` and its edge goes dashed, which is what a dash already
 * means here — `.ui-ring-unmeasured`, `.ui-runpipe-unreached` — *this is not a live fact*. No
 * tone, because `.ui-note-stale` settled the house style: say stale without borrowing a colour
 * that would claim something about the quota instead of about the reading of it.
 *
 * **It is never the only thing that says a number, and it no longer says a name.** The provider's
 * name was printed beside its rings and is now a mark inside them: at this size the word cost more
 * room than the drawing it labelled. What carries the reading instead is text that is worth more —
 * unfolded, every window prints its own percentage and when it reopens. The name is still in the
 * ring's sentence for assistive tech, and still in the hover text over the slot. Colour reinforces
 * and never states.
 */
export function QuotaNotch({ host = "contained", onMove }: QuotaNotchProps) {
  const quota = useQuota();
  const [phase, setPhase] = useState<Phase>("folded");
  const timer = useRef<number | undefined>(undefined);
  const drawing = useRef<HTMLDivElement>(null);
  // The drawing's height folded, for the contained host to hang from (`app.css`,
  // `.quota-notch-contained`). Measured rather than derived: it is two rings or three, a border
  // and some padding, and a sum of tokens written out here would be one more thing to keep equal.
  const [rest, setRest] = useState<number | undefined>(undefined);
  const count = quota.data?.providers.length ?? 0;

  useEffect(() => () => window.clearTimeout(timer.current), []);

  // Before paint, so the first frame is already hung from the right place.
  useLayoutEffect(() => {
    if (phase !== "folded" || drawing.current === null) return;
    setRest(drawing.current.getBoundingClientRect().height);
  }, [phase, count]);

  // Nothing until the first answer. A notch drawn empty would say "nothing is burned", which is the
  // most misleading thing this feature could claim — and it would say it at exactly the moment
  // nobody has measured anything yet.
  if (quota.data === undefined || quota.data.providers.length === 0) return null;

  const { providers, source, unreachable } = quota.data;
  // One instant for the whole drawing. Read once rather than per line, so two windows in the same
  // notch cannot be counted against two different nows — a difference of milliseconds that shows
  // up as "resets in 1h" beside "resets in 59min".
  const now = Date.now();
  const reached = phase !== "folded";

  const unfold = () => {
    window.clearTimeout(timer.current);
    setPhase("open");
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
      style={rest === undefined ? undefined : ({ "--quota-notch-rest": `${rest}px` } as CSSProperties)}
      onPointerEnter={unfold}
      onPointerLeave={() => fold(LINGER_MS)}
      onFocus={unfold}
      onBlur={(event) => {
        // Focus has nothing to slip off, so it lingers for nothing — but the panel still plays out.
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) fold(0);
      }}
    >
      {providers.map((provider) => (
        <div className="quota-notch-slot" key={provider.provider} title={titleOf(provider)}>
          <Ring
            label={provider.provider}
            tracks={tracksOf(provider)}
            mark={<ProviderMark provider={provider.provider} />}
          />
          {reached && (
            <div className="quota-notch-detail">
              {tracksOf(provider).map((track) => (
                <span className="quota-notch-line" key={track.name}>
                  <span className="quota-notch-window">{track.name}</span>
                  {/*
                    The dash takes a class of its own because it is not a figure. In the weight
                    and the ink the percentages wear, an em dash reads as a value somebody
                    measured; what it means is that nobody could.
                  */}
                  <span
                    className={
                      track.measured
                        ? "quota-notch-percent"
                        : "quota-notch-percent quota-notch-percent-absent"
                    }
                  >
                    {track.measured ? `${Math.round(track.used * 100)}%` : "—"}
                  </span>
                  <span className="quota-notch-reset">{resetPhrase(provider, track.name, now)}</span>
                </span>
              ))}
              {/*
                How the figure was come by, and how old it is, on one line — the two facts that
                decide what the percentages above are worth. `derived` is named rather than hidden
                because it means something the owner has to weigh: that reading only moves when
                they run something, so an hour-old one is normal and an hour-old `official` one is
                not.
              */}
              <span className="quota-notch-fidelity">{fidelityPhrase(provider, now)}</span>
            </div>
          )}
        </div>
      ))}
      {source === "stored" && reached && (
        /*
          The last known figures, with the sidecar unreachable. Said rather than implied: these
          numbers are real and they are old, and a reader who takes them for live ones is reading a
          quota that may have moved a long way since.
        */
        <span className="quota-notch-stale" title={unreachable}>
          last known
        </span>
      )}
      {onMove !== undefined && (
        /*
          Folded, the notch is the rings and nothing else, in BOTH hosts — the owner's call: a pin
          drawn under two rings at rest was one more thing on the edge of every page, for an action
          taken once. The contained host used to be the exception, drawn in both states as the way
          out to the floating one, and that way is still one hover away.

          Folded, the control is still THERE, tucked into `.sr-only` rather than left unrendered.
          Rendered only when unfolded it could not be reached by a keyboard at all: the wrapper's
          `onFocus` fires from a child, and the only focusable child is this button, so focus would
          have nowhere to land and the notch would never unfold. Tucked away it is out of flow — it
          measures nothing, so neither the page nor the window the Rust side fits round this drawing
          changes size — and it is a tab stop, which unfolds the notch and brings itself into view.
          The wrapper, drawn, is what rules it off from the rings above it: stacked in the same
          column, at the same size, a control reads as a third provider (`app.css`).

          What that does NOT buy in the floating host: reaching its window from the keyboard in the
          first place. It is built `skip_taskbar(true)` (`notch.rs`), which on Windows means
          WS_EX_TOOLWINDOW and no place in the Alt+Tab order, and `focused(false)`, so it never
          takes focus by itself. Focus arrives when the owner clicks the notch, or through assistive
          tech that can move it; Alt+F4 closes the window — which docks it — only once focus is
          already there. So an owner working from the keyboard alone cannot reach the way back at
          all, and the main window has no other: `AppShell` draws no notch while the mode is
          `global`, by design, and the mode has no home in settings yet. That gap is named here
          rather than implied away — closing it is a decision about where such a control belongs in
          the app, not a line of this component.
        */
        <span className={reached ? "quota-notch-control" : "sr-only"}>
          {host === "contained" ? (
            <IconButton label="Keep the notch in front of every window" icon={Pin} onClick={onMove} />
          ) : (
            <IconButton label="Put the notch back inside NucleOS" icon={PinOff} onClick={onMove} />
          )}
        </span>
      )}
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

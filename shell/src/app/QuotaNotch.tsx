import { useState } from "react";
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
 * **One component, two hosts.** Nothing about the reading changes between them — only which way
 * the move control points. The contained host is also the recoil if the floating window misbehaves
 * (risk R1), which is why it keeps working with no Rust side at all.
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
  const [reached, setReached] = useState(false);

  // Nothing until the first answer. A notch drawn empty would say "nothing is burned", which is the
  // most misleading thing this feature could claim — and it would say it at exactly the moment
  // nobody has measured anything yet.
  if (quota.data === undefined || quota.data.providers.length === 0) return null;

  const { providers, source, unreachable } = quota.data;
  // One instant for the whole drawing. Read once rather than per line, so two windows in the same
  // notch cannot be counted against two different nows — a difference of milliseconds that shows
  // up as "resets in 1h" beside "resets in 59min".
  const now = Date.now();

  return (
    <div
      className={`quota-notch quota-notch-${host}`}
      data-unfolded={reached}
      onPointerEnter={() => setReached(true)}
      onPointerLeave={() => setReached(false)}
      onFocus={() => setReached(true)}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setReached(false);
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
                  <span className="quota-notch-percent">
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
      {onMove !== undefined &&
        (host === "contained" ? (
          <IconButton label="Keep the notch in front of every window" icon={Pin} onClick={onMove} />
        ) : (
          /*
            Folded, the floating notch is the rings and nothing else — but the way back is still
            THERE, tucked into `.sr-only` rather than left unrendered. Rendered only when unfolded,
            it could not be reached by a keyboard at all: the wrapper's `onFocus` fires from a
            child, and the only focusable child was this button, so focus had nowhere to land and
            the notch never unfolded. Tucked away it is out of flow — it measures nothing, so the
            window the Rust side fits round this drawing is the same size it was — and it is the
            first tab stop, which unfolds the notch and brings itself into view.

            What that does NOT buy, and the old comment here claimed: reaching this window from the
            keyboard in the first place. It is built `skip_taskbar(true)` (`notch.rs`), which on
            Windows means WS_EX_TOOLWINDOW and no place in the Alt+Tab order, and `focused(false)`,
            so it never takes focus by itself. Focus arrives when the owner clicks the notch, or
            through assistive tech that can move it; Alt+F4 closes the window — which docks it —
            only once focus is already there. So an owner working from the keyboard alone cannot
            reach this control at all, and the main window has no other: `AppShell` draws no notch
            while the mode is `global`, by design, and the mode has no home in settings yet. That
            gap is named here rather than implied away — closing it is a decision about where such
            a control belongs in the app, not a line of this component.
          */
          <span className={reached ? "quota-notch-back" : "sr-only"}>
            <IconButton label="Put the notch back inside NucleOS" icon={PinOff} onClick={onMove} />
          </span>
        ))}
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
  const head = `${provider.provider} — ${provider.fidelity}`;
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

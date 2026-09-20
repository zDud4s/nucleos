import { useState } from "react";
import { Pin, PinOff } from "lucide-react";
import { useQuota, type QuotaProvider, type QuotaWindow } from "../data/quota";
import { IconButton, Ring, type RingTrack } from "../ui";
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
   * Which host is drawing it (design D8). `contained` is always unfolded — it lives inside a page
   * with room to spare. `global` floats over every other window, so at rest it is the rings alone
   * and it unfolds when the pointer reaches it, or when focus does. Focus is the weaker of the two
   * there, and deliberately said so: the floating window is outside the Alt+Tab order, so focus
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
 * How much of each assistant's usage window is gone, drawn at the top edge.
 *
 * **One component, two hosts.** The main window draws it contained, at the top of the page area;
 * the notch window draws it floating, over everything (`NotchWindow`). Nothing about the reading
 * changes between them — only whether it is folded at rest, and which way the move control points.
 * The contained host is also the recoil if the floating window misbehaves (risk R1), which is why
 * it keeps working with no Rust side at all.
 *
 * **It is never the only thing that says a number.** The rings carry the colour, and each one
 * carries a sentence for assistive tech and a hover title per arc; unfolded, the provider's name is
 * printed beside them as text. Colour reinforces and never states.
 */
export function QuotaNotch({ host = "contained", onMove }: QuotaNotchProps) {
  const quota = useQuota();
  const [reached, setReached] = useState(false);

  // Nothing until the first answer. A notch drawn empty would say "nothing is burned", which is the
  // most misleading thing this feature could claim — and it would say it at exactly the moment
  // nobody has measured anything yet.
  if (quota.data === undefined || quota.data.providers.length === 0) return null;

  const { providers, source, unreachable } = quota.data;
  const unfolded = host === "contained" || reached;

  return (
    <div
      className={`quota-notch quota-notch-${host}`}
      data-unfolded={unfolded}
      onPointerEnter={() => setReached(true)}
      onPointerLeave={() => setReached(false)}
      onFocus={() => setReached(true)}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setReached(false);
      }}
    >
      {providers.map((provider) => (
        <div className="quota-notch-slot" key={provider.provider}>
          <Ring label={provider.provider} tracks={tracksOf(provider)} />
          {unfolded && (
            <span className="quota-notch-name" title={titleOf(provider)}>
              {provider.provider}
            </span>
          )}
        </div>
      ))}
      {source === "stored" && unfolded && (
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
          <span className={unfolded ? "quota-notch-back" : "sr-only"}>
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
 * The hover text: the fidelity, the age, and each window in words.
 *
 * `derived` is named rather than hidden, because it means something the owner has to weigh: that
 * reading only moves when they run something, so an hour-old one is normal and an hour-old
 * `official` one is not.
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

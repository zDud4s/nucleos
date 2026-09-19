import { useQuota, type QuotaProvider, type QuotaWindow } from "../data/quota";
import { Ring, type RingTrack } from "../ui";

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
 * How much of each assistant's usage window is gone, drawn at the top edge.
 *
 * **Contained inside the app, which is phase 1's whole position on the question.** The owner asked
 * for this to be configurable — global and always in front, or only inside NucleOS — and the
 * second window is a phase of its own because the risk in it is real: a borderless always-on-top
 * webview on `windows-gnu` has to be proven before anything is built on top of it. Contained works
 * today, and it is the fallback if that spike fails.
 *
 * **It is never the only thing that says a number.** The rings carry the colour, and each one
 * carries a sentence for assistive tech and a hover title per arc; the provider's name is printed
 * beside them as text. Colour reinforces and never states.
 */
export function QuotaNotch() {
  const quota = useQuota();

  // Nothing until the first answer. A notch drawn empty would say "nothing is burned", which is the
  // most misleading thing this feature could claim — and it would say it at exactly the moment
  // nobody has measured anything yet.
  if (quota.data === undefined || quota.data.providers.length === 0) return null;

  const { providers, source, unreachable } = quota.data;

  return (
    <div className="quota-notch">
      {providers.map((provider) => (
        <div className="quota-notch-slot" key={provider.provider}>
          <Ring label={provider.provider} tracks={tracksOf(provider)} />
          <span className="quota-notch-name" title={titleOf(provider)}>
            {provider.provider}
          </span>
        </div>
      ))}
      {source === "stored" && (
        /*
          The last known figures, with the sidecar unreachable. Said rather than implied: these
          numbers are real and they are old, and a reader who takes them for live ones is reading a
          quota that may have moved a long way since.
        */
        <span className="quota-notch-stale" title={unreachable}>
          last known
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

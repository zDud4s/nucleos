import { useHealth } from "../data/system";

/**
 * Is the núcleo answering? One line, three readings.
 *
 * Three and not two: *checking* is a real state and it is not the same as
 * *unreachable*. The probe takes as long as it takes, and a line that shows
 * "unreachable" for the first few hundred milliseconds of every cold start
 * teaches people to ignore it — which is exactly the line you cannot afford to
 * have ignored.
 *
 * A word, not only a dot. The dot is the peripheral signal; the word is what
 * the roughly one person in twelve who cannot separate the hues reads instead.
 */
export function ConnectionStatus() {
  const health = useHealth();
  const reachable = health.data;

  const tone = reachable === undefined ? "checking" : reachable ? "up" : "down";
  const text =
    reachable === undefined ? "checking the daemon" : reachable ? "daemon connected" : "daemon unreachable";

  return (
    <p className={`app-connection app-connection-${tone}`} role="status">
      <span className="app-connection-dot" aria-hidden="true" />
      <span className="app-connection-text">{text}</span>
    </p>
  );
}

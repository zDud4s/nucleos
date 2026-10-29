import { useState, type FormEvent } from "react";
import { Button } from "../ui/Button";
import { ErrorNote } from "../ui/ErrorNote";
import { Field } from "../ui/Field";
import { pair, type RemoteSession } from "./session";

export function defaultDeviceName(): string {
  const platform = typeof navigator === "undefined" ? "" : navigator.platform;
  return platform.trim() === "" ? "Browser" : platform.trim();
}

/** Keep what a pairing code can contain; the daemon normalises, this only makes the field forgiving. */
function tidy(raw: string): string {
  return raw.replace(/\s+/g, "").toUpperCase();
}

/**
 * The screen before the app: enter the code the desktop shows (`XXXXX-XXXXX`) and name this browser.
 */
export function Pairing({ onPaired }: { onPaired: (session: RemoteSession) => void }) {
  const [code, setCode] = useState("");
  const [deviceName, setDeviceName] = useState(defaultDeviceName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (busy || tidy(code) === "") return;
    setBusy(true);
    setError(null);
    const result = await pair(tidy(code), deviceName);
    setBusy(false);
    switch (result.kind) {
      case "ok":
        onPaired(result.session);
        return;
      case "invalid_code":
        setError("That code was not accepted. Check it against the one on the desktop, or generate a new one.");
        return;
      case "slow_down":
        setError(
          result.retryAfter === null
            ? "Too many attempts. Wait a moment and try again."
            : `Too many attempts. Try again in ${result.retryAfter} seconds.`,
        );
        return;
      case "network":
        setError("The gateway could not be reached. Check the connection and try again.");
        return;
    }
  }

  return (
    <main className="remote-pairing">
      <form onSubmit={submit} className="remote-pairing-form" aria-label="Pair this browser">
        <h1>Pair this browser</h1>
        <p>Enter the code shown in NucleOS on your computer, under Remote devices.</p>
        <Field label="Pairing code">
          <input
            value={code}
            onChange={(event) => setCode(event.target.value)}
            placeholder="XXXXX-XXXXX"
            autoComplete="off"
            autoCapitalize="characters"
            spellCheck={false}
          />
        </Field>
        <Field label="Device name">
          <input value={deviceName} onChange={(event) => setDeviceName(event.target.value)} autoComplete="off" />
        </Field>
        {error === null ? null : <ErrorNote>{error}</ErrorNote>}
        <Button type="submit" variant="primary" disabled={busy || tidy(code) === ""}>
          {busy ? "Pairing…" : "Pair"}
        </Button>
      </form>
    </main>
  );
}

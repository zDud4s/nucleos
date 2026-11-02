import { useEffect, useState } from "react";
import { creationOptions, credentialJson, requestOptions } from "./StepUp";

type Ticket = { device_name: string; label: string; code: string; registered: boolean; can_register: boolean };
type Begin = { kind: "register" | "verify"; publicKey: Record<string, unknown> };

const headers = { "Content-Type": "application/json", "X-Nucleos-Csrf": "1" };
const invalid = "This step-up ticket is no longer valid.";

function ticketFromLocation(): string | null {
  const value = new URLSearchParams(window.location.search).get("ticket");
  return value === null || value === "" ? null : value;
}

export function TicketStepUp() {
  const [ticket] = useState(ticketFromLocation);
  const [info, setInfo] = useState<Ticket | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (ticket === null) { setMessage(invalid); return; }
    void fetch(`/auth/stepup/info?ticket=${encodeURIComponent(ticket)}`, { headers: { "X-Nucleos-Csrf": "1" } })
      .then(async (response) => response.ok ? setInfo((await response.json()) as Ticket) : setMessage(invalid))
      .catch(() => setMessage("The gateway could not be reached."));
  }, [ticket]);
  async function continueStepUp() {
    if (ticket === null || busy) return;
    setBusy(true);
    try {
      const begun = await fetch("/auth/stepup/begin", { method: "POST", headers, body: JSON.stringify({ ticket }) });
      if (!begun.ok) { setMessage(begun.status === 410 ? invalid : "The passkey could not be prepared."); return; }
      const body = (await begun.json()) as Begin;
      const credential = body.kind === "register"
        ? await navigator.credentials.create({ publicKey: creationOptions(body.publicKey) })
        : await navigator.credentials.get({ publicKey: requestOptions(body.publicKey) });
      if (credential === null) { setMessage("The passkey was not confirmed. Nothing was done."); return; }
      const finished = await fetch("/auth/stepup/finish", { method: "POST", headers, body: JSON.stringify({ ticket, credential: credentialJson(credential as PublicKeyCredential) }) });
      setMessage(finished.ok ? "Step-up confirmed. Return to the app." : finished.status === 410 ? invalid : "The passkey was refused. Nothing was done.");
    } catch {
      setMessage("The passkey was not confirmed. Nothing was done.");
    } finally { setBusy(false); }
  }
  if (message !== null) return <main className="remote-pairing"><section className="remote-pairing-form"><p>{message}</p></section></main>;
  if (info === null) return <main className="remote-pairing"><section className="remote-pairing-form"><p>Loading step-up request…</p></section></main>;
  const unavailable = !info.registered && !info.can_register;
  return <main className="remote-pairing"><section className="remote-pairing-form">
    <h1>Confirm this action</h1><p>Device: {info.device_name}</p><p>Action: {info.label}</p><p>Comparison code: <strong>{info.code}</strong></p>
    <p>Continue only if this matches what the app shows and you started this request.</p>
    {unavailable ? <p>Pair again on that machine to get another 15 minutes to register its first passkey.</p> : <button type="button" disabled={busy} onClick={() => void continueStepUp()}>{busy ? "Waiting for the passkey…" : info.registered ? "Confirm with passkey" : "Register and confirm with passkey"}</button>}
  </section></main>;
}

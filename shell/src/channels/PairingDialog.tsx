import { useEffect, useRef, useState } from "react";
import { DAEMON_NAME } from "../copy";
import { ApiRefusal } from "../data/client";
import { useCancelPairing, useStartPairing, type Channel, type PairingCode } from "../data/channels";
import { Button, CopyButton, ErrorNote, Modal, RefusalNote } from "../ui";
import "./pairing.css";

export interface PairingDialogProps {
  channel: Channel;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

const SENTENCES = {
  channel_not_configured: "this channel has no secrets yet: add the channel's secrets first, then ask for a code",
};

function clock(ms: number): string {
  const total = Math.max(0, Math.ceil(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

/**
 * Asks the daemon for a pairing code when it opens and shows it with the command to send.
 *
 * The code leaves the daemon only in the POST answer (annex E.4), so it lives in this component's
 * `useState` and nowhere else: not the react-query cache (the mutation is reset as soon as the code
 * is taken), not storage, not logs. Closing cancels the code; reopening asks for a new one.
 *
 * The request is guarded by a ref that marks "asked during this opening" and is cleared when `open`
 * goes back to false. Refs survive StrictMode's simulated remount, so the effect cannot post twice.
 * The expiry countdown is local clock arithmetic against `expires_at`; the live state of the code
 * (paired, exhausted, ...) belongs to the status poll, which is not wired here.
 */
export function PairingDialog({ channel, open, onOpenChange }: PairingDialogProps) {
  const start = useStartPairing(channel.id);
  const cancel = useCancelPairing(channel.id);
  const [code, setCode] = useState<PairingCode | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const asked = useRef(false);

  const { mutateAsync, reset } = start;
  useEffect(() => {
    if (!open) {
      asked.current = false;
      setCode(null);
      return;
    }
    if (asked.current) return;
    asked.current = true;
    mutateAsync().then(
      (pairing) => {
        setCode(pairing);
        setNow(Date.now());
        // Drop the mutation's own copy of the answer; only this state keeps the code.
        reset();
      },
      () => {
        // The error stays on the mutation, where the render below reads it.
      },
    );
  }, [open, mutateAsync, reset]);

  useEffect(() => {
    if (!open || code === null) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [open, code]);

  function close() {
    if (code !== null) cancel.mutate();
    setCode(null);
    onOpenChange(false);
  }

  const remaining = code === null ? 0 : Date.parse(code.expires_at) - now;
  const error = start.error;

  return (
    <Modal
      open={open}
      onOpenChange={(next) => {
        if (next) onOpenChange(true);
        else close();
      }}
      title={`Pair a conversation on ${channel.label}`}
      size="sm"
      footer={
        <Button variant="ghost" onClick={close}>
          Cancel
        </Button>
      }
    >
      {code === null && error === null && <p role="status">asking for a code…</p>}
      {error instanceof ApiRefusal && <RefusalNote refusal={error} sentences={SENTENCES} />}
      {error !== null && !(error instanceof ApiRefusal) && (
        <ErrorNote>the {DAEMON_NAME} did not answer the request for a code</ErrorNote>
      )}
      {code !== null && (
        <div className="pairing-code">
          <p>Send this to the bot in the conversation you want to pair:</p>
          <div className="pairing-code-command">
            <code>{code.command}</code>
            <CopyButton value={code.command} label="the pairing command" />
          </div>
          <p className="pairing-code-digits">{code.code}</p>
          {remaining > 0 ? (
            <p className="pairing-countdown">
              expires in <span>{clock(remaining)}</span>
            </p>
          ) : (
            <p className="pairing-expired">expired: close this and ask for a new code</p>
          )}
          {channel.state === "offline" && (
            <p>the bot only answers when the adapter is back online; the code is valid until then</p>
          )}
        </div>
      )}
    </Modal>
  );
}

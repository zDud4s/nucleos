import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import type { Channel, ChannelState } from "../data/channels";
import { sidecarKeyOf, useRestartSidecar } from "../data/system";
import { Button } from "../ui";
import { ConfirmButton } from "../ui/ConfirmButton";
import { RefusalNote } from "../ui/RefusalNote";
import { RelativeTime } from "../ui/readings";
import { CapabilityList } from "./CapabilityList";
import { IdentitiesPanel } from "./IdentitiesPanel";
import { PairingDialog } from "./PairingDialog";
import { PrimaryChannelControl } from "./PrimaryChannelControl";
import "./card.css";

const STATE: Record<ChannelState, { label: string; sentence: string }> = {
  not_configured: { label: "not configured", sentence: "this channel is missing what it needs to start" },
  starting: { label: "starting", sentence: "waiting for the adapter to say hello" },
  online: { label: "online", sentence: "connected and delivering" },
  offline: { label: "offline", sentence: "not connected right now; the outbox holds what is waiting" },
  incompatible: {
    label: "incompatible",
    sentence:
      "the adapter spoke a contract the background service refused (409 contract_version): update the adapter, then restart it",
  },
};

/**
 * Short copy of System's private `RestartSidecar`. The page keeps its own
 * version private, so the card carries the brief form instead of importing it.
 */
function RestartButton({ name }: { name: string }) {
  const restart = useRestartSidecar();
  return (
    <>
      <ConfirmButton
        label="Restart"
        confirmLabel="Start it again"
        variant="quiet"
        disabled={restart.isPending}
        onConfirm={() => restart.mutate(name)}
      />
      {restart.isSuccess ? (
        <span className="ui-quiet" role="status">asked: the supervisor is trying now</span>
      ) : null}
      {restart.isError ? (
        isApiRefusal(restart.error) ? (
          <RefusalNote refusal={restart.error} sentences={{ running: "it is running now: there was nothing to start" }} />
        ) : (
          <span className="ui-quiet">could not ask the supervisor</span>
        )
      ) : null}
    </>
  );
}

/** One messaging channel: its state, what it can do, what is queued or dropped, and the primary choice. */
export function ChannelCard({ channel, primary }: { channel: Channel; primary: string | null }) {
  const state = STATE[channel.state];
  const key = sidecarKeyOf(channel.health_row);
  const { outbox, drops } = channel;
  const configured = channel.state !== "not_configured";
  const [pairing, setPairing] = useState(false);

  return (
    <section className="ch-card" aria-label={channel.label}>
      <header className="ch-card-head">
        <h3 className="ch-card-title">{channel.label}</h3>
        <span className="ch-card-state" data-state={channel.state}>{state.label}</span>
        {channel.since !== null && configured ? <RelativeTime at={channel.since} /> : null}
        {channel.adapter !== null ? <span className="ui-quiet">contract {channel.adapter.contract_version}</span> : null}
      </header>
      <p className="ui-quiet">{state.sentence}</p>

      {!configured ? (
        <div>
          <p>Missing secrets:</p>
          <ul>
            {channel.missing_secrets.map((s) => (
              <li key={s}>{s}</li>
            ))}
          </ul>
          <Link to="/system/$view" params={{ view: "settings" }}>
            Machine settings: {channel.settings_area}
          </Link>
        </div>
      ) : (
        <>
          <CapabilityList capabilities={channel.capabilities} />
          <ul className="ch-card-facts">
            <li>
              outbox: {outbox.pending} pending, {outbox.leased} leased, {outbox.failed} failed
            </li>
            {outbox.pending > 0 && outbox.oldest_due_at !== null ? (
              <li>
                oldest waiting <RelativeTime at={outbox.oldest_due_at} />
              </li>
            ) : null}
            <li>
              {drops.unknown_sender} from unknown senders since <RelativeTime at={drops.counted_since} />
            </li>
            {drops.pairing_failures > 0 ? <li>{drops.pairing_failures} failed pairing attempts</li> : null}
          </ul>
        </>
      )}

      <div className="ch-card-actions">
        <Link to="/system/$view" params={{ view: "health" }}>
          {channel.health_row} in health
        </Link>
        {key !== null && configured ? <RestartButton name={key} /> : null}
        {configured ? (
          <Button variant="quiet" onClick={() => setPairing(true)}>
            Pair
          </Button>
        ) : null}
      </div>
      {configured && channel.pairing !== null ? (
        // Only the existence of the code is shown; the code itself lives in the dialog.
        <p className="ui-quiet">a pairing code is waiting</p>
      ) : null}
      {configured ? <PrimaryChannelControl channel={channel} primary={primary} /> : null}
      {configured ? <IdentitiesPanel channel={channel} primary={channel.id === primary} /> : null}
      {configured ? <PairingDialog channel={channel} open={pairing} onOpenChange={setPairing} /> : null}
    </section>
  );
}

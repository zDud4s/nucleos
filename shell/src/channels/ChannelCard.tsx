import { useState } from "react";
import { Link } from "@tanstack/react-router";
import type { Channel, ChannelState } from "../data/channels";
import { sidecarKeyOf } from "../data/system";
import { Button, Callout, Panel, Quiet, StateBadge } from "../ui";
import { RelativeTime } from "../ui/readings";
import { CapabilityList } from "./CapabilityList";
import { IdentitiesPanel } from "./IdentitiesPanel";
import { PairingDialog } from "./PairingDialog";
import { PrimaryChannelControl } from "./PrimaryChannelControl";
import { RestartSidecar } from "./RestartSidecar";
import "./card.css";

/** What each state means in a sentence. Its tone and label are the state map's (`channel`). */
const SENTENCE: Record<ChannelState, string> = {
  not_configured: "this channel is missing what it needs to start",
  starting: "waiting for the adapter to say hello",
  online: "connected and delivering",
  offline: "not connected right now; the outbox holds what is waiting",
  incompatible:
    "the adapter spoke a contract the background service refused (409 contract_version): update the adapter, then restart it",
};

/** One messaging channel: its state, what it can do, what is queued or dropped, and the primary choice. */
export function ChannelCard({ channel, primary }: { channel: Channel; primary: string | null }) {
  const key = sidecarKeyOf(channel.health_row);
  const { outbox, drops } = channel;
  const configured = channel.state !== "not_configured";
  const [pairing, setPairing] = useState(false);

  return (
    <Panel
      title={channel.label}
      aside={
        <>
          <StateBadge domain="channel" state={channel.state} />
          {channel.since !== null && configured ? <RelativeTime at={channel.since} /> : null}
          {channel.adapter !== null ? (
            <span className="ch-card-meta">contract {channel.adapter.contract_version}</span>
          ) : null}
        </>
      }
    >
      <div className="ch-card">
        <Quiet compact says={SENTENCE[channel.state]} />

        {!configured ? (
          <Callout tone="pending" title="Missing secrets">
            <ul className="ch-card-missing">
              {channel.missing_secrets.map((s) => (
                <li key={s}>{s}</li>
              ))}
            </ul>
            <Link to="/system/$view" params={{ view: "settings" }}>
              Machine settings: {channel.settings_area}
            </Link>
          </Callout>
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
          {configured ? (
            <Button variant="ghost" onClick={() => setPairing(true)}>
              Pair
            </Button>
          ) : null}
          {key !== null && configured ? <RestartSidecar name={key} /> : null}
          <span className="ch-card-health">
            <Link to="/system/$view" params={{ view: "health" }}>
              {channel.health_row} in health
            </Link>
          </span>
        </div>
        {configured && channel.pairing !== null ? (
          // Only the existence of the code is shown; the code itself lives in the dialog.
          <Quiet compact says="a pairing code is waiting" />
        ) : null}
        {configured ? <PrimaryChannelControl channel={channel} primary={primary} /> : null}
        {configured ? <IdentitiesPanel channel={channel} primary={channel.id === primary} /> : null}
        {configured ? <PairingDialog channel={channel} open={pairing} onOpenChange={setPairing} /> : null}
      </div>
    </Panel>
  );
}

import { useState } from "react";
import { Link } from "@tanstack/react-router";
import type { Channel, ChannelState } from "../data/channels";
import { sidecarKeyOf } from "../data/system";
import { Button, Callout, Quiet, StateBadge, Tip } from "../ui";
import { RelativeTime } from "../ui/readings";
import { CapabilityList } from "./CapabilityList";
import { ChannelCards, ChannelDetail, ChannelTiles, type ChannelTile } from "./ChannelDetail";
import { IdentitiesPanel } from "./IdentitiesPanel";
import { PairingDialog } from "./PairingDialog";
import { PrimaryChannelControl } from "./PrimaryChannelControl";
import { RestartSidecar } from "./RestartSidecar";
import "./card.css";

/** The word shown in the Status tile; the sentence below opens from its tip. */
const LABEL: Record<ChannelState, string> = {
  not_configured: "Not configured",
  starting: "Starting",
  online: "Connected",
  offline: "Offline",
  incompatible: "Incompatible",
};

/** What each state means in a sentence. Its tone and label are the state map's (`channel`). */
const SENTENCE: Record<ChannelState, string> = {
  not_configured: "this channel is missing what it needs to start",
  starting: "waiting for the adapter to say hello",
  online: "connected and delivering",
  offline: "not connected right now; the outbox holds what is waiting",
  incompatible:
    "the adapter spoke a contract the background service refused (409 contract_version): update the adapter, then restart it",
};

/**
 * One messaging channel's detail, in the shared `ChannelDetail` frame: a header with its state and its
 * actions, a row of tiles (status, outbox, drops), and cards for who is paired where, the projects
 * and the capabilities it lacks.
 */
export function ChannelCard({ channel, primary }: { channel: Channel; primary: string | null }) {
  const key = sidecarKeyOf(channel.health_row);
  const { outbox, drops } = channel;
  const configured = channel.state !== "not_configured";
  const [pairing, setPairing] = useState(false);

  const tiles: ChannelTile[] = [
    { term: "Status", value: LABEL[channel.state], tip: SENTENCE[channel.state] },
  ];
  if (configured) {
    tiles.push(
      { term: "Pending", value: outbox.pending },
      { term: "Leased", value: outbox.leased },
      { term: "Failed", value: outbox.failed > 0 ? <span className="chd-count-bad">{outbox.failed}</span> : outbox.failed },
    );
    if (outbox.stale_acks !== undefined) tiles.push({ term: "Stale acks", value: outbox.stale_acks });
    if (outbox.stale_renews !== undefined) tiles.push({ term: "Stale renews", value: outbox.stale_renews });
    if (outbox.pending > 0 && outbox.oldest_due_at !== null) {
      tiles.push({ term: "Oldest waiting", value: <RelativeTime at={outbox.oldest_due_at} /> });
    }
    tiles.push({
      term: "Unknown senders",
      value: drops.unknown_sender,
      tip: (
        <>
          counted since <RelativeTime at={drops.counted_since} />
        </>
      ),
    });
    if (drops.unknown_sender_limited !== undefined) {
      tiles.push({ term: "Rate-limited", value: drops.unknown_sender_limited });
    }
    if (drops.pairing_failures > 0) tiles.push({ term: "Failed pairings", value: drops.pairing_failures });
  }

  return (
    <ChannelDetail
      title={channel.label}
      state={<StateBadge domain="channel" state={channel.state} />}
      meta={channel.since !== null && configured ? <RelativeTime at={channel.since} /> : undefined}
      // The contract and the two places this channel is read elsewhere, behind one mark.
      about={
        <Tip
          name="About this channel"
          persist
          tip={
            <span className="ch-about">
              {channel.adapter !== null ? <span>contract {channel.adapter.contract_version}</span> : null}
              <Link to="/system/$view" params={{ view: "health" }} title={`${channel.health_row} in health`}>
                Health
              </Link>
              <Link to="/system/$view" params={{ view: "settings" }} title={`channels/${channel.id}`}>
                Edit in Machine
              </Link>
            </span>
          }
        />
      }
      actions={
        configured ? (
          <>
            <Button variant="ghost" onClick={() => setPairing(true)}>
              Pair
            </Button>
            {key !== null ? <RestartSidecar name={key} /> : null}
            <PrimaryChannelControl channel={channel} primary={primary} />
          </>
        ) : undefined
      }
    >
      <ChannelTiles label="Channel facts" tiles={tiles} />

      {configured && channel.pairing !== null ? (
        // Only the existence of the code is shown; the code itself lives in the dialog.
        <Quiet compact says="a pairing code is waiting" />
      ) : null}

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
      ) : null}

      {configured ? (
        <>
          <ChannelCards>
            <IdentitiesPanel channel={channel} primary={channel.id === primary} />
            <CapabilityList capabilities={channel.capabilities} />
          </ChannelCards>
          <PairingDialog channel={channel} open={pairing} onOpenChange={setPairing} />
        </>
      ) : null}
    </ChannelDetail>
  );
}

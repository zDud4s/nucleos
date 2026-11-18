import { useState, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import type { Channel, ChannelState } from "../data/channels";
import { sidecarKeyOf } from "../data/system";
import { Button, Callout, Panel, Quiet, StateBadge, Tip } from "../ui";
import { RelativeTime } from "../ui/readings";
import { CapabilityList } from "./CapabilityList";
import { IdentitiesPanel } from "./IdentitiesPanel";
import { PairingDialog } from "./PairingDialog";
import { PrimaryChannelControl } from "./PrimaryChannelControl";
import { RestartSidecar } from "./RestartSidecar";
import "./card.css";

/** The word shown in the Status fact; the sentence below opens from its tip. */
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

/** One labelled value of the facts strip. */
function Fact({ term, tip, children }: { term: string; tip?: ReactNode; children: ReactNode }) {
  return (
    <div className="sy-fact">
      <dt>{term}</dt>
      <dd>
        {children}
        {tip === undefined ? null : (
          <>
            {" "}
            <Tip name={`About ${term.toLowerCase()}`} persist tip={tip} />
          </>
        )}
      </dd>
    </div>
  );
}

/**
 * One messaging channel's detail: a header with its state and its actions, a strip of facts
 * (status, outbox, drops), the capabilities it lacks, and who is paired where.
 */
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
          <span className="ch-state">
            <StateBadge domain="channel" state={channel.state} />
            {channel.since !== null && configured ? (
              <span className="ch-card-meta">
                <RelativeTime at={channel.since} />
              </span>
            ) : null}
            {/* The contract and the two places this channel is read elsewhere, behind one mark. */}
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
          </span>
          {configured ? (
            <span className="ch-actions">
              <Button variant="ghost" onClick={() => setPairing(true)}>
                Pair
              </Button>
              {key !== null ? <RestartSidecar name={key} /> : null}
              <PrimaryChannelControl channel={channel} primary={primary} />
            </span>
          ) : null}
        </>
      }
    >
      <div className="ch-card">
        <dl className="sy-strip ch-facts ch-card-wide" aria-label="Channel facts">
          <Fact term="Status">
            {LABEL[channel.state]}{" "}
            <Tip name={`About ${channel.state.replace("_", " ")}`} persist tip={SENTENCE[channel.state]} />
          </Fact>
          {configured ? (
            <>
              <Fact term="Pending">{outbox.pending}</Fact>
              <Fact term="Leased">{outbox.leased}</Fact>
              <Fact term="Failed">{outbox.failed > 0 ? <span className="ch-count-bad">{outbox.failed}</span> : outbox.failed}</Fact>
              {outbox.stale_acks !== undefined ? <Fact term="Stale acks">{outbox.stale_acks}</Fact> : null}
              {outbox.stale_renews !== undefined ? <Fact term="Stale renews">{outbox.stale_renews}</Fact> : null}
              {outbox.pending > 0 && outbox.oldest_due_at !== null ? (
                <Fact term="Oldest waiting">
                  <RelativeTime at={outbox.oldest_due_at} />
                </Fact>
              ) : null}
              <Fact
                term="Unknown senders"
                tip={
                  <>
                    counted since <RelativeTime at={drops.counted_since} />
                  </>
                }
              >
                {drops.unknown_sender}
              </Fact>
              {drops.unknown_sender_limited !== undefined ? (
                <Fact term="Rate-limited">{drops.unknown_sender_limited}</Fact>
              ) : null}
              {drops.pairing_failures > 0 ? <Fact term="Failed pairings">{drops.pairing_failures}</Fact> : null}
            </>
          ) : null}
        </dl>

        {configured && channel.pairing !== null ? (
          // Only the existence of the code is shown; the code itself lives in the dialog.
          <div className="ch-card-wide">
            <Quiet compact says="a pairing code is waiting" />
          </div>
        ) : null}

        {!configured ? (
          <div className="ch-card-wide">
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
          </div>
        ) : null}

        {/* Two columns when the detail is wide: who is paired on the left, what the channel lacks on the right. */}
        {configured ? <IdentitiesPanel channel={channel} primary={channel.id === primary} /> : null}
        {configured ? <CapabilityList capabilities={channel.capabilities} /> : null}
        {configured ? <PairingDialog channel={channel} open={pairing} onOpenChange={setPairing} /> : null}
      </div>
    </Panel>
  );
}

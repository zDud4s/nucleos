import { ChannelCard } from "../channels/ChannelCard";
import { useChannels, type RoutingState } from "../data/channels";
import { isApiRefusal } from "../data/client";
import { DAEMON_NAME } from "../copy";
import { ErrorNote, RefusalNote } from "../ui";
import "./channels.css";

/** One sentence per routing problem; `ok` has none, so nothing is drawn for it. */
const ROUTING: Record<Exclude<RoutingState, "ok">, string> = {
  no_primary: "There is no primary channel: notifications have nowhere to be sent by default.",
  primary_not_configured: "The primary channel is not configured yet: finish its setup first.",
  primary_without_home: "The primary channel has no home conversation: pair it or pick a home.",
};

/** System > Channels: the routing warning, then one card per channel in the núcleo's order. */
export function ChannelsView() {
  const channels = useChannels();

  if (channels.isError) {
    if (isApiRefusal(channels.error)) return <RefusalNote refusal={channels.error} />;
    return (
      <ErrorNote onRetry={() => void channels.refetch()}>
        the {DAEMON_NAME} is not responding — nothing is known about the channels
      </ErrorNote>
    );
  }
  if (!channels.data) {
    return <p className="sy-note" role="status">reading the channels…</p>;
  }

  const { routing, primary, channels: list } = channels.data;
  return (
    <div className="ch-page">
      {routing.state !== "ok" ? (
        <p className="ch-routing-warning" role="note">{ROUTING[routing.state]}</p>
      ) : null}
      {list.length === 0 ? (
        <p className="sy-note">no channel adapters are built into this daemon</p>
      ) : (
        <div className="ch-grid">
          {list.map((channel) => (
            <ChannelCard key={channel.id} channel={channel} primary={primary} />
          ))}
        </div>
      )}
    </div>
  );
}

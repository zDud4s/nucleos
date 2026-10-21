import { ChannelCard } from "../channels/ChannelCard";
import { useChannels, type RoutingState } from "../data/channels";
import { isApiRefusal } from "../data/client";
import { DAEMON_NAME } from "../copy";
import { Callout, ErrorNote, Quiet, RefusalNote, Skeleton } from "../ui";
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
        the {DAEMON_NAME} is not responding: nothing is known about the channels
      </ErrorNote>
    );
  }
  if (!channels.data) {
    return <Skeleton label="Reading the channels" variant="cards" count={3} />;
  }

  const { routing, primary, channels: list } = channels.data;
  return (
    <div className="ch-page">
      {routing.state !== "ok" ? (
        <Callout tone="pending">{ROUTING[routing.state]}</Callout>
      ) : null}
      {list.length === 0 ? (
        <Quiet says="no channel adapters are built into this daemon" />
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

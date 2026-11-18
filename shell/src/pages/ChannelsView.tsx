import { useState, type ReactNode } from "react";
import { ChannelCard } from "../channels/ChannelCard";
import { useChannels, type RoutingState } from "../data/channels";
import { isApiRefusal } from "../data/client";
import { DAEMON_NAME } from "../copy";
import { Badge, ErrorNote, Panel, Quiet, RefusalNote, Row, Rows, Skeleton, StateBadge, Tip } from "../ui";
import "./channels.css";

/** One sentence per routing problem; `ok` has none, so nothing is drawn for it. */
const ROUTING: Record<Exclude<RoutingState, "ok">, string> = {
  no_primary: "There is no primary channel: notifications have nowhere to be sent by default, so a parked question or a long turn shows only in the app.",
  primary_not_configured: "The primary channel is not configured yet: finish its setup first.",
  primary_without_home: "The primary channel has no home conversation: pair it or pick a home.",
};

/**
 * A running configuration listed beside the channels (Email, Voice, Calendar): the page that owns
 * the readout hands over its row's badge and its detail, so this view never reads them itself.
 */
export interface ChannelConfigEntry {
  id: string;
  label: string;
  /** What the row shows after the name: a state badge, or nothing. */
  badge?: ReactNode;
  /** The detail drawn when the row is selected. */
  detail: ReactNode;
  /** Drawn beside the detail's title, e.g. a `Tip`. */
  aside?: ReactNode;
}

/** One selectable line of the list, whichever kind of entry it stands for. */
interface ListEntry {
  id: string;
  label: string;
  badge: ReactNode;
  primary: boolean;
}

/**
 * System > Channels: a list of every channel and running configuration on the left, the selected
 * one's detail on the right. The routing warning sits behind a tip beside the list's heading.
 */
export function ChannelsView({ configs = [] }: { configs?: ChannelConfigEntry[] }) {
  const channels = useChannels();
  const [picked, setPicked] = useState<string | null>(null);

  if (!channels.isError && !channels.data) {
    return (
      <div className="ch-page">
        {/* The list and the detail of the first channel, at their loaded heights. */}
        <Skeleton label="Reading the channels" variant="cards" gridClassName="ch-layout" heights={["14rem", "30rem"]} />
      </div>
    );
  }

  // A failed read of the channels does not take the running configurations with it: they come from
  // other routes, so the note stands above whatever else can still be shown.
  let note: ReactNode = null;
  if (channels.isError) {
    note = isApiRefusal(channels.error) ? (
      <RefusalNote refusal={channels.error} />
    ) : (
      <ErrorNote onRetry={() => void channels.refetch()}>
        the {DAEMON_NAME} is not responding: nothing is known about the channels
      </ErrorNote>
    );
  }
  const routing = channels.data?.routing ?? { state: "ok" as const };
  const primary = channels.data?.primary ?? null;
  const list = channels.data?.channels ?? [];
  const entries: ListEntry[] = [
    ...list.map((channel) => ({
      id: channel.id,
      label: channel.label,
      badge: <StateBadge domain="channel" state={channel.state} />,
      primary: channel.id === primary,
    })),
    ...configs.map((config) => ({ id: config.id, label: config.label, badge: config.badge, primary: false })),
  ];
  if (entries.length === 0) return note ?? <Quiet says="no channel adapters are built into this daemon" />;

  // The choice survives a refetch; a choice that no longer exists falls back to the default.
  const fallback = entries.find((entry) => entry.primary) ?? entries[0]!;
  const selected = entries.find((entry) => entry.id === picked) ?? fallback;
  const channel = list.find((c) => c.id === selected.id);
  const config = configs.find((c) => c.id === selected.id);

  return (
    <div className="ch-page">
      {note}
      <div className="ch-layout">
        <Panel
          title="Channels"
          aside={
            routing.state !== "ok" ? <Tip name="Routing problem" persist tip={ROUTING[routing.state]} /> : undefined
          }
        >
          <Rows label="Channel list">
            {entries.map((entry) => {
              const current = entry.id === selected.id;
              return (
                <Row key={entry.id} dense current={current}>
                  <button
                    type="button"
                    className="ch-pick"
                    aria-current={current ? "true" : undefined}
                    onClick={() => setPicked(entry.id)}
                  >
                    <span className="ch-pick-name">{entry.label}</span>
                    {entry.badge}
                    {entry.primary ? <Badge tone="info">primary</Badge> : null}
                  </button>
                </Row>
              );
            })}
          </Rows>
        </Panel>
        <div className="ch-detail">
          {channel !== undefined ? (
            <ChannelCard key={channel.id} channel={channel} primary={primary} />
          ) : config !== undefined ? (
            <Panel title={config.label} aside={config.aside}>{config.detail}</Panel>
          ) : null}
        </div>
      </div>
    </div>
  );
}

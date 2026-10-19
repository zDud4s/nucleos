import { useMemo } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import {
  useDebounced,
  useGlobalSearch,
  type SearchGroup,
  type SearchHit,
  type SearchSourceId,
} from "../data/search";
import { usePaletteGroup, usePaletteQuery, type PaletteGroup } from "../ui";

/** How long the typing must pause before the palette asks six places at once. */
const PAUSE_MS = 200;

/** Where Enter goes. Typed loosely on purpose: a hit carries the route as a plain string. */
type Go = (hit: SearchHit) => void;

/**
 * What the palette finds beyond pages: conversations, runs, the feed, mail, saved web pages and
 * the owner's notes, matched by the daemon over the text each one keeps.
 *
 * Mounted once next to `Destinations`. It draws nothing itself — each source becomes a
 * `prematched` palette group (the daemon already matched it, over text this list does not hold),
 * and a source with no rows registers no group, so no heading is drawn over nothing.
 */
export function GlobalSearch() {
  const navigate = useNavigate();
  const query = usePaletteQuery();
  const paused = useDebounced(query, PAUSE_MS);
  const onChats = useRouterState({
    select: (state) => state.location.pathname.startsWith("/chats"),
  });
  // The Chats page registers its own "Said in a conversation" group, drawn with the turn it will
  // scroll to; a second one here would be the same rows twice. Skipped before asking, too.
  const skip = useMemo<SearchSourceId[]>(() => (onChats ? ["said"] : []), [onChats]);
  const groups = useGlobalSearch(paused, { skip });

  const go = useMemo<Go>(
    () => (hit) => {
      // The hit's route is built from daemon ids, not from the router's own table.
      const target = hit.search === undefined ? { to: hit.to } : { to: hit.to, search: hit.search };
      void navigate(target as never);
    },
    [navigate],
  );

  const seen = new Map(groups.map((group) => [group.id, group]));
  return (
    <>
      {(["said", "runs", "feed", "mail", "web", "notes"] as const).map((id) => (
        <SourceGroup key={id} id={id} group={seen.get(id) ?? null} go={go} />
      ))}
    </>
  );
}

function SourceGroup({ id, group, go }: { id: SearchSourceId; group: SearchGroup | null; go: Go }) {
  const palette = useMemo<PaletteGroup | null>(
    () =>
      group === null
        ? null
        : {
            id: `search-${id}`,
            heading: group.heading,
            prematched: true,
            items: group.hits.map((hit) => ({
              id: hit.id,
              label: <HitRow hit={hit} />,
              match: `${hit.title} ${hit.text}`,
              run: () => go(hit),
            })),
          },
    [id, group, go],
  );
  usePaletteGroup(palette);
  return null;
}

/** Two lines, plain text: the title and where it is from, then the excerpt. */
function HitRow({ hit }: { hit: SearchHit }) {
  return (
    <span className="ui-palette-hit">
      <span className="ui-palette-hit-head">
        <span className="ui-palette-hit-title">{hit.title}</span>
        {hit.detail !== undefined && hit.detail !== "" && (
          <span className="ui-palette-hit-detail">{hit.detail}</span>
        )}
      </span>
      {hit.text !== "" && <span className="ui-palette-hit-text">{hit.text}</span>}
    </span>
  );
}

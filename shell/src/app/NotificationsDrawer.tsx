// §spec mapa-do-projeto
import { useEffect, useRef, useState } from "react";
import { Bell } from "lucide-react";
import {
  isHeld,
  useRecentFeed,
  usePendingNotifications,
  type FeedEntry,
  type PendingNotification,
} from "../data/feed";
import { KindBadge } from "../pages/Feed";
import { ErrorNote, RelativeTime } from "../ui";

/**
 * The notifications drawer: what the calendar is holding, and the last lines of
 * the feed.
 *
 * **It is a place to go and look, not a toast** (design §3.2). Nothing here
 * appears by itself, nothing covers the page you are reading, and nothing
 * disappears after four seconds carrying the only copy of a fact. That is the
 * whole design decision: a feed entry *is* a notification (§6.19), so the
 * notification surface has to be a record you can open, not an interruption you
 * have to catch.
 *
 * Held and delivered are kept apart, on `delivered_at` and nothing else. The
 * núcleo keeps delivered rows on purpose — the fear this feature earns is *did
 * the calendar swallow something?*, and a queue that listed only what has not
 * arrived yet cannot answer it. Merging the two lists on screen would throw
 * that away just as thoroughly as dropping the rows would.
 */
export function NotificationsDrawer() {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);

  /**
   * The held count is on the trigger, so this one polls whether the drawer is
   * open or not — a badge that only appeared once you opened the panel would
   * summon nobody. `POLL.queue` is the cadence for exactly this: it moves when
   * a notification is written or when the calendar opens, and both are events.
   *
   * The feed window behind it is the opposite case, and is `enabled` only while
   * the drawer is open: this component is mounted on every page in the app, and
   * a hook that fetched while shut would poll the feed for the whole session on
   * behalf of a panel nobody has looked at.
   */
  const pending = usePendingNotifications();
  const recent = useRecentFeed({ enabled: open });

  const notifications = pending.data ?? [];
  const held = notifications.filter(isHeld);
  const delivered = notifications.filter((notification) => !isHeld(notification));

  function close() {
    setOpen(false);
    // Focus goes back where it came from. A drawer that closes and leaves focus
    // on nothing strands anybody navigating by keyboard at the top of the page.
    trigger.current?.focus();
  }

  useEffect(() => {
    if (!open) return undefined;
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") close();
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open]);

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className="app-notify"
        aria-expanded={open}
        /*
          Spelled out, because the count beside the word would otherwise be
          announced as "Notifications3" — adjacent inline text concatenates with
          no separator. A number on screen and not in the accessible name is a
          summons only some people get.
        */
        aria-label={
          held.length === 0
            ? "Notifications, nothing held"
            : `Notifications, ${held.length} held by the calendar`
        }
        onClick={() => setOpen((current) => !current)}
      >
        {/*
          A mark, like every row above it. This control is shaped like a
          `nav-item` on purpose — it sits in the rail and reads as one more place
          to go — and when the rail's rows swapped their two-letter monograms for
          lucide icons this was the one row left spelling `Nt`, which read as a
          row that had lost its icon rather than as a row with a different kind.
        */}
        <Bell className="app-notify-glyph" strokeWidth={1.5} aria-hidden="true" />
        <span className="app-notify-label">Notifications</span>
        {held.length > 0 && (
          <span className="app-notify-count" aria-hidden="true">
            {held.length}
          </span>
        )}
      </button>

      {open && (
        <aside className="app-drawer" aria-label="Notifications">
          <div className="app-drawer-head">
            <h2 className="app-drawer-title">Notifications</h2>
            <button type="button" className="app-drawer-close" onClick={close} aria-label="Close notifications">
              <span aria-hidden="true">×</span>
            </button>
          </div>

          <div className="app-drawer-body">
            {pending.isError && (
              <ErrorNote>the núcleo did not answer — what is held is not known</ErrorNote>
            )}

            <section className="app-drawer-section">
              <h3 className="app-drawer-heading">Held by the calendar</h3>
              {held.length === 0 ? (
                <p className="app-drawer-empty">
                  Nothing is being held. A notification waits here only while the calendar says you
                  are in something.
                </p>
              ) : (
                <ul className="app-drawer-list" aria-label="Held notifications">
                  {held.map((notification) => (
                    <NotificationRow key={notification.id} notification={notification} />
                  ))}
                </ul>
              )}
            </section>

            <section className="app-drawer-section">
              <h3 className="app-drawer-heading">Held, then delivered</h3>
              {delivered.length === 0 ? (
                <p className="app-drawer-empty">
                  Nothing has been held and let through yet. These are kept so that “did it ever
                  arrive?” stays answerable.
                </p>
              ) : (
                <ul className="app-drawer-list" aria-label="Delivered notifications">
                  {delivered.map((notification) => (
                    <NotificationRow key={notification.id} notification={notification} />
                  ))}
                </ul>
              )}
            </section>

            <section className="app-drawer-section">
              <h3 className="app-drawer-heading">Latest feed</h3>
              {recent.isError && (
                <ErrorNote>the núcleo did not answer — the feed could not be read</ErrorNote>
              )}
              {recent.data === undefined && !recent.isError && (
                <p className="app-drawer-empty">reading the feed…</p>
              )}
              {recent.data !== undefined && recent.data.length === 0 && (
                <p className="app-drawer-empty">The feed is empty.</p>
              )}
              {recent.data !== undefined && recent.data.length > 0 && (
                <ul className="app-drawer-list" aria-label="Latest feed">
                  {recent.data.map((entry) => (
                    <FeedLine key={entry.id} entry={entry} />
                  ))}
                </ul>
              )}
            </section>
          </div>
        </aside>
      )}
    </>
  );
}

/**
 * One notification, with the two timestamps that make its state readable.
 *
 * `queued_at` alone cannot say whether something arrived; `delivered_at` alone
 * cannot say how long it waited. Both, or the row is only half a record.
 */
function NotificationRow({ notification }: { notification: PendingNotification }) {
  return (
    <li className="app-drawer-row">
      <div className="app-drawer-row-head">
        <KindBadge kind={notification.kind} />
        <span className="app-drawer-when">
          queued <RelativeTime at={notification.queued_at} />
        </span>
        {notification.delivered_at !== null && (
          <span className="app-drawer-when">
            delivered <RelativeTime at={notification.delivered_at} />
          </span>
        )}
      </div>
      <p className="app-drawer-summary">{notification.summary}</p>
    </li>
  );
}

function FeedLine({ entry }: { entry: FeedEntry }) {
  return (
    <li className="app-drawer-row">
      <div className="app-drawer-row-head">
        <KindBadge kind={entry.kind} />
        <RelativeTime at={entry.created_at} />
      </div>
      <p className="app-drawer-summary">{entry.summary}</p>
    </li>
  );
}

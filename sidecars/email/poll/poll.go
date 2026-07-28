// Package poll is the sidecar's one loop: read what is new, deliver it, let the núcleo move the
// cursor.
package poll

import (
	"log"
	"time"

	imapv2 "github.com/emersion/go-imap/v2"

	"nucleosemail/config"
	"nucleosemail/daemon"
	"nucleosemail/extract"
	"nucleosemail/imap"
)

// MaxPerBatch matches the núcleo's ceiling. Paging is this side's job: a batch past it is rejected
// with a 400 rather than silently truncated, which is the behaviour that makes the limit real.
const MaxPerBatch = 200

// ResyncWindow is how far back a mailbox is read when the stored position means nothing — no
// cursor at all, or a UIDVALIDITY change that rebuilt the server's uid space. The núcleo's own
// backfill cutoff decides what is worth triaging out of that; this bound only stops a first sync
// from walking a decade of archive.
const ResyncWindow = 7 * 24 * time.Hour

// Fetcher is the one thing the collector needs from a mailbox, so the rule that decides how far the
// cursor may move can be tested without a server.
type Fetcher interface {
	Fetch(uid imapv2.UID) (imap.Raw, error)
}

// Collect reads messages in uid order and reports how far it actually got.
//
// `maxExamined` is the highest uid whose fetch COMPLETED and produced a decision — a delivered
// message or a recorded skip. It is deliberately NOT the highest uid the search returned: a
// connection that dies at the twelfth of fifty must report the twelfth, or the núcleo advances its
// cursor over thirty-eight messages nobody ever read, and they are gone with no way to notice.
//
// A message that cannot be parsed is a DECISION, not a failure: it is recorded as skipped, the
// watermark passes it, and the user is told. Otherwise one corrupt message stalls the mailbox
// forever.
func Collect(fetcher Fetcher, uids []imapv2.UID, limit int) (
	messages []daemon.Message,
	skipped []daemon.Skipped,
	maxExamined uint32,
) {
	for index, uid := range uids {
		if index >= limit {
			break
		}
		raw, err := fetcher.Fetch(uid)
		if err != nil {
			// The connection, not the message. Stop here and report only what was read.
			log.Printf("email: fetch of uid %d failed (%v) — stopping this pass", uid, err)
			break
		}

		message, err := extract.Message(raw.Body, raw.UID, raw.InternalDate)
		if err != nil {
			skipped = append(skipped, daemon.Skipped{
				UID:    raw.UID,
				Reason: err.Error(),
			})
		} else {
			messages = append(messages, message)
		}
		if raw.UID > maxExamined {
			maxExamined = raw.UID
		}
	}
	return messages, skipped, maxExamined
}

// lastSeen reports the stored position for logging, with 0 standing for "no cursor yet".
func lastSeen(cursor *daemon.Cursor) uint32 {
	if cursor == nil {
		return 0
	}
	return cursor.LastUID
}

// Once performs a single poll: connect, read what is new, deliver it.
func Once(cfg config.Config, client *daemon.Client) error {
	cursor, err := client.GetCursor(cfg.Mailbox)
	if err != nil {
		return err
	}

	conn, err := imap.Dial(cfg.Addr(), cfg.Username, cfg.Password)
	if err != nil {
		return err
	}
	defer conn.Close()

	uidValidity, err := conn.Select(cfg.Mailbox)
	if err != nil {
		return err
	}

	var uids []imapv2.UID
	if cursor == nil || cursor.UIDValidity != uidValidity {
		// The stored position is not a position any more, so fall back to a time window.
		uids, err = conn.SearchSince(time.Now().Add(-ResyncWindow))
	} else {
		uids, err = conn.SearchAbove(cursor.LastUID)
	}
	if err != nil {
		return err
	}
	if len(uids) == 0 {
		// Said out loud, every time. A poll that finds nothing used to be silent, which made
		// silence mean both "connected, nothing new" and "never connected at all" — and those are
		// the two things a person setting this up most needs to tell apart.
		log.Printf("email: %s has nothing new above uid %d", cfg.Mailbox, lastSeen(cursor))
		return nil
	}
	log.Printf("email: %s has %d message(s) to read", cfg.Mailbox, len(uids))

	messages, skipped, maxExamined := Collect(conn, uids, MaxPerBatch)
	if maxExamined == 0 {
		// Nothing was read all the way through; delivering would move the cursor over mail nobody
		// examined.
		return nil
	}

	result, err := client.Deliver(daemon.Batch{
		Mailbox:        cfg.Mailbox,
		UIDValidity:    uidValidity,
		MaxUIDExamined: maxExamined,
		Skipped:        skipped,
		Messages:       messages,
	})
	if err != nil {
		// The cursor lives in the núcleo, so a failed delivery simply repeats next pass. There is
		// no local state to roll back — which is the point of keeping the cursor there.
		return err
	}

	log.Printf(
		"email: delivered %d message(s), %d duplicate(s), %d skipped; cursor now %d",
		result.Ingested, result.Duplicates, len(skipped), result.Cursor,
	)
	return nil
}

// Run polls until the process is stopped. Errors are logged and retried on the next tick: the
// daemon supervises this process, and a mailbox that is briefly unreachable is not an emergency.
func Run(cfg config.Config, client *daemon.Client) {
	for {
		if err := Once(cfg, client); err != nil {
			log.Printf("email: poll failed: %v", err)
		}
		time.Sleep(cfg.PollInterval)
	}
}

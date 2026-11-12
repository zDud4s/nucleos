package adapter

import (
	"context"
	"log"

	"nucleoschannelkit/backoff"
	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
	"nucleoschannelkit/kit"

	"nucleostelegram/config"
	"nucleostelegram/telegram"
)

// pollTimeoutSecs is the getUpdates long-poll timeout, as in v1.
const pollTimeoutSecs = 50

// PollBot is the slice of the Bot API client the poll loop needs; telegram.Client satisfies it.
type PollBot interface {
	GetUpdatesContext(ctx context.Context, offset int64, timeoutSecs int) ([]telegram.Update, error)
}

// Poster delivers one inbound event to the core; *kit.Sink satisfies it.
type Poster interface {
	Post(ctx context.Context, ev contract.Event) (kit.Outcome, error)
}

// Attacher fills in the media parts of an event whose update carried media (AttachMedia behind an
// interface so the loop can be tested without a Bot API).
type Attacher interface {
	Attach(ctx context.Context, ev *contract.Event, msg *telegram.Message) error
}

// MediaAttacher is the production Attacher: it downloads from Telegram and uploads to the core.
type MediaAttacher struct {
	Bot FileBot
	Up  Uploader
}

// Attach implements Attacher with AttachMedia.
func (m MediaAttacher) Attach(ctx context.Context, ev *contract.Event, msg *telegram.Message) error {
	return AttachMedia(ctx, ev, msg, m.Bot, m.Up)
}

// Poll long-polls getUpdates and hands each update to the core, one at a time and in order.
//
// The offset policy is the opposite of v1: it is persisted only after the core settled the update
// (Outcome.Advance), so a crash or shutdown mid-delivery redelivers the same update with the same
// event_id and the core deduplicates it. Updates that cannot be translated, and permanent rejections
// (which the Sink reports as Advance), also move the offset so one bad update never blocks the cursor.
//
// Poll returns nil when ctx ends, and an error when the Sink gives up without ctx ending (a fatal
// 401/403/409); in both cases the offset has not moved past the unsettled update.
func Poll(ctx context.Context, bot PollBot, att Attacher, sink Poster, offsetPath string, clk clock.Clock) error {
	offset := config.LoadOffset(offsetPath)
	retry := backoff.Updates.Base
	wait := func() bool {
		ok := backoff.Sleep(ctx, clk, retry)
		retry = backoff.Next(retry, backoff.Updates.Base, backoff.Updates.Max)
		return ok
	}

	for ctx.Err() == nil {
		updates, err := bot.GetUpdatesContext(ctx, offset, pollTimeoutSecs)
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			log.Printf("getUpdates error: %v", err)
			if !wait() {
				return nil
			}
			continue
		}
		retry = backoff.Updates.Base

		for _, u := range updates {
			advance, err := handleUpdate(ctx, att, sink, u)
			if err != nil {
				if ctx.Err() != nil {
					return nil
				}
				return err
			}
			if !advance {
				// Transient (media could not be carried): poll the same offset again after a pause.
				if ctx.Err() != nil || !wait() {
					return nil
				}
				break
			}
			offset = u.UpdateID + 1
			if err := config.SaveOffset(offsetPath, offset); err != nil {
				log.Printf("could not persist update offset: %v", err)
			}
		}
	}
	return nil
}

// handleUpdate translates and delivers one update. advance reports whether the offset may move past
// it. A non-nil error is the Sink giving up (or ctx ending inside Post).
func handleUpdate(ctx context.Context, att Attacher, sink Poster, u telegram.Update) (advance bool, err error) {
	var (
		ev         contract.Event
		needsMedia bool
		ok         bool
	)
	if u.CallbackQuery != nil {
		var reason string
		ev, ok, reason = InboundCallback(*u.CallbackQuery)
		if !ok {
			log.Printf("dropping callback query in update %d: %s", u.UpdateID, reason)
		}
	} else {
		ev, needsMedia, ok = Inbound(u)
	}
	if !ok {
		return true, nil
	}
	if needsMedia {
		if err := att.Attach(ctx, &ev, u.Message); err != nil {
			if ctx.Err() == nil {
				log.Printf("attaching media of update %d: %v", u.UpdateID, err)
			}
			return false, nil
		}
	}
	out, err := sink.Post(ctx, ev)
	if err != nil {
		return false, err
	}
	return out.Advance, nil
}

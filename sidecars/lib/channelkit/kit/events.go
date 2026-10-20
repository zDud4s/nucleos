package kit

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"

	"nucleoschannelkit/backoff"
	"nucleoschannelkit/contract"
)

// Outcome tells the adapter what to do with its platform cursor after Post.
type Outcome struct {
	// Advance: the event is settled (delivered, a duplicate, dropped, or
	// permanently rejected), so the adapter may persist its offset past it.
	Advance bool
}

// Sink delivers inbound events to the core with at-least-once semantics.
type Sink struct {
	client *Client
	stats  *Stats
}

// NewSink builds a Sink over a Client; stats counts permanent failures.
func NewSink(c *Client, stats *Stats) *Sink {
	return &Sink{client: c, stats: stats}
}

// Post sends ev to POST /channels/{channel}/events.
//
//   - 2xx (accepted, duplicate, dropped): Advance.
//   - 400/413/422: permanent. Advance and count a failure, otherwise one
//     malformed update would block the platform cursor forever.
//   - network error, 408, 429, 5xx: the same event_id is sent again with
//     backoff.Updates until it settles or ctx ends. If ctx ends, Post returns
//     ctx.Err() without Advance, so the event comes back after a restart.
//   - 401/403/409 and anything else unexpected: an error wrapping ErrFatal.
//
// An event that fails local validation is never sent: it counts as a
// permanent failure and Advance is set.
func (s *Sink) Post(ctx context.Context, ev contract.Event) (Outcome, error) {
	payload, err := json.Marshal(ev)
	if err == nil {
		_, err = contract.DecodeStrict[contract.Event](payload)
	}
	if err != nil {
		s.stats.PermanentFailures.Add(1)
		return Outcome{Advance: true}, nil
	}

	pol := backoff.Updates
	delay := pol.Base
	for {
		if err := ctx.Err(); err != nil {
			return Outcome{}, err
		}
		body, status, err := s.client.do(ctx, http.MethodPost, s.client.channelPath("/events"), bytes.NewReader(payload), "application/json")
		if err != nil {
			if ctx.Err() != nil {
				return Outcome{}, ctx.Err()
			}
			status = 0
		}
		switch Classify(EndpointEvents, status, body) {
		case OK:
			return Outcome{Advance: true}, nil
		case SkipEvent:
			s.stats.PermanentFailures.Add(1)
			return Outcome{Advance: true}, nil
		case Retry:
			if !backoff.Sleep(ctx, s.client.clock, delay) {
				return Outcome{}, ctx.Err()
			}
			delay = backoff.Next(delay, pol.Base, pol.Max)
		default:
			return Outcome{}, fmt.Errorf("%w: events: status %d", ErrFatal, status)
		}
	}
}

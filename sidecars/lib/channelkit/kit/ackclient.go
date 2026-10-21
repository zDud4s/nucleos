package kit

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net/http"
	"strconv"

	"nucleoschannelkit/backoff"
	"nucleoschannelkit/contract"
)

// ErrAckRejected: the core permanently rejected an ack (400/404); it is not repeated.
var ErrAckRejected = errors.New("channelkit: ack rejected")

// AckStateLost is the State of an AckResult when the core no longer leases the action
// (409 not_leased): the lease expired and the action will be redelivered, so there is
// nothing left to acknowledge.
const AckStateLost contract.AckState = "lost"

// AckResult is the outcome of an ack. A late ack (Ignored) is a success: the core
// already settled the action, and the kit neither re-executes nor logs an error.
type AckResult struct {
	State   contract.AckState
	Ignored bool
}

// WithStats returns a copy of the client that counts rejected and late acks in s.
func (c *Client) WithStats(s *Stats) *Client {
	cp := *c
	cp.stats = s
	return &cp
}

// Ack sends one ack (POST /channels/{channel}/outbox/{id}/ack), without retrying.
//
//   - 2xx: the AckResult of the response (Ignored is success).
//   - 409 not_leased: AckResult{State: "lost"} and no error.
//   - 400/404: an error wrapping ErrAckRejected, counted in Stats.AckRejected.
//   - network error, 408, 429, 5xx: an error wrapping ErrTransient.
//   - 401/403 and anything else unexpected: an error wrapping ErrFatal.
//
// A cancelled ctx returns its own error.
func (c *Client) Ack(ctx context.Context, id uint64, ack contract.Ack) (AckResult, error) {
	payload, err := ack.Marshal()
	if err != nil {
		return AckResult{}, fmt.Errorf("%w: encode ack: %w", ErrFatal, err)
	}
	path := c.channelPath("/outbox/" + strconv.FormatUint(id, 10) + "/ack")
	body, status, err := c.do(ctx, http.MethodPost, path, bytes.NewReader(payload), "application/json")
	if err != nil {
		if ctx.Err() != nil {
			return AckResult{}, fmt.Errorf("ack %d: %w", id, ctx.Err())
		}
		return AckResult{}, fmt.Errorf("%w: ack %d: %w", ErrTransient, id, err)
	}
	switch Classify(EndpointAck, status, body) {
	case OK:
		resp, derr := contract.DecodeAckResponse(body)
		if derr != nil {
			return AckResult{}, fmt.Errorf("%w: ack %d response: %w", ErrFatal, id, derr)
		}
		if resp.Ignored && c.stats != nil {
			c.stats.LateAcks.Add(1)
		}
		return AckResult{State: resp.State, Ignored: resp.Ignored}, nil
	case StopRenewing:
		return AckResult{State: AckStateLost}, nil
	case Retry:
		return AckResult{}, fmt.Errorf("%w: ack %d: status %d", ErrTransient, id, status)
	case GiveUp:
		if c.stats != nil {
			c.stats.AckRejected.Add(1)
		}
		return AckResult{}, fmt.Errorf("%w: ack %d: status %d", ErrAckRejected, id, status)
	default:
		return AckResult{}, fmt.Errorf("%w: ack %d: status %d", ErrFatal, id, status)
	}
}

// AckWithRetry is Ack repeated with backoff.Outbox on transient failures (network error,
// 408, 429, 5xx) until it settles or ctx ends, in which case it returns ctx's error. A
// 409 not_leased settles as AckResult{State: "lost"} without error; a rejected ack
// (ErrAckRejected) or a fatal one (ErrFatal) is returned at once.
func (c *Client) AckWithRetry(ctx context.Context, id uint64, ack contract.Ack) (AckResult, error) {
	delay := backoff.Outbox.Base
	for {
		res, err := c.Ack(ctx, id, ack)
		if err == nil || !errors.Is(err, ErrTransient) {
			return res, err
		}
		if !backoff.Sleep(ctx, c.clock, delay) {
			return AckResult{}, fmt.Errorf("ack %d: %w", id, ctx.Err())
		}
		delay = backoff.Next(delay, backoff.Outbox.Base, backoff.Outbox.Max)
	}
}

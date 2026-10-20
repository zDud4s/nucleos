package kit

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"strconv"
	"time"

	"nucleoschannelkit/contract"
)

// pollSlack is added to the server-side wait to form the request deadline.
// It is a variable so tests can shorten it.
var pollSlack = 10 * time.Second

// logf reports dropped actions; tests replace it.
var logf = log.Printf

// Item is one outbox entry: either a valid Action, or a Rescue ack for an
// action the kit could not read (Action is nil then). ID is the outbox row id
// in both cases.
type Item struct {
	Action contract.Action
	Rescue *contract.Ack
	ID     uint64
}

// Batch is the result of one outbox poll. Typing lists the conversations that
// need a typing indicator (ephemeral: no id, no ack).
type Batch struct {
	Items  []Item
	Typing []string
}

// Poll performs one long-poll of the outbox (GET /channels/{channel}/outbox?wait=N).
// The request carries a deadline of wait plus a slack. One unreadable action never
// fails the batch: it becomes a rescue item, or is dropped and logged when it has
// no usable id. A malformed envelope or a network error yields ErrTransient, a
// fatal status yields ErrFatal, and a cancelled ctx returns its own error.
func (c *Client) Poll(ctx context.Context, waitSecs int) (Batch, error) {
	if waitSecs < 0 {
		waitSecs = 0
	}
	rctx, cancel := context.WithTimeout(ctx, time.Duration(waitSecs)*time.Second+pollSlack)
	defer cancel()
	path := c.channelPath("/outbox") + "?wait=" + strconv.Itoa(waitSecs)
	body, status, err := c.do(rctx, http.MethodGet, path, nil, "")
	if err != nil {
		if ctx.Err() != nil {
			return Batch{}, fmt.Errorf("outbox poll: %w", ctx.Err())
		}
		return Batch{}, fmt.Errorf("%w: outbox poll: %w", ErrTransient, err)
	}
	switch Classify(EndpointOutbox, status, body) {
	case OK:
	case Retry:
		return Batch{}, fmt.Errorf("%w: outbox poll: status %d", ErrTransient, status)
	default:
		return Batch{}, fmt.Errorf("%w: outbox poll: status %d", ErrFatal, status)
	}
	resp, err := contract.DecodeOutboxResponse(body)
	if err != nil {
		return Batch{}, fmt.Errorf("%w: outbox response: %w", ErrTransient, err)
	}
	var out Batch
	for _, raw := range resp.Actions {
		action, rescue, derr := contract.DecodeAction(raw)
		switch {
		case derr == nil:
			out.Items = append(out.Items, Item{Action: action, ID: action.ActionID()})
		case rescue != nil:
			out.Items = append(out.Items, Item{Rescue: rescue, ID: rawID(raw)})
		default:
			logf("channelkit: dropping outbox action without a usable id: %v", derr)
		}
	}
	for _, t := range resp.Ephemeral {
		out.Typing = append(out.Typing, t.Conversation)
	}
	return out, nil
}

// rawID reads the id of an action DecodeAction already judged to have a usable one.
func rawID(raw []byte) uint64 {
	var head struct {
		ID json.RawMessage `json:"id"`
	}
	if json.Unmarshal(raw, &head) != nil {
		return 0
	}
	id, _ := strconv.ParseUint(string(bytes.TrimSpace(head.ID)), 10, 64)
	return id
}

package contract

import (
	"bytes"
	"encoding/json"
	"strconv"
)

// Lane is the delivery lane of an outbox action (spec section 6.1).
type Lane int

const (
	// Ordered actions are delivered one by one, in outbox order.
	Ordered Lane = iota
	// Immediate actions skip the queue and expire quickly.
	Immediate
)

// String names the lane.
func (l Lane) String() string {
	if l == Immediate {
		return "Immediate"
	}
	return "Ordered"
}

// Lane returns Ordered for send.
func (Send) Lane() Lane { return Ordered }

// Lane returns Ordered for edit.
func (Edit) Lane() Lane { return Ordered }

// Lane returns Immediate for answer_action.
func (AnswerAction) Lane() Lane { return Immediate }

// Rescue ack reasons (T11).
const (
	ReasonInvalidAction     = "invalid_action"
	ReasonUnsupportedAction = "unsupported_action"
)

// DecodeAction decodes one outbox action. When the action is unreadable but its
// id is usable (an integer >= 1), it also returns the rescue ack the kit must send
// so the core does not retry forever: a permanent failure, "unsupported_action" for
// an unknown type and "invalid_action" for anything else. Without a usable id there
// is no ack (the kit logs and drops the action).
func DecodeAction(raw []byte) (Action, *Ack, error) {
	a, err := ParseAction(raw)
	if err == nil {
		return a, nil, nil
	}
	var head struct {
		ID   json.RawMessage `json:"id"`
		Type json.RawMessage `json:"type"`
	}
	if json.Unmarshal(raw, &head) != nil {
		return nil, nil, err
	}
	if id, perr := strconv.ParseUint(string(bytes.TrimSpace(head.ID)), 10, 64); perr != nil || id == 0 {
		return nil, nil, err
	}
	reason := ReasonInvalidAction
	var typ string
	if json.Unmarshal(head.Type, &typ) == nil {
		switch ActionType(typ) {
		case ActionSend, ActionEdit, ActionAnswerAction:
		default:
			reason = ReasonUnsupportedAction
		}
	}
	return nil, &Ack{Failed: &Failed{Retryable: false, Reason: reason}}, err
}

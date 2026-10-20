package contract

import (
	"encoding/json"
	"fmt"
)

// TypingKind is the only ephemeral type there is.
type TypingKind string

// EphemeralTyping is the "typing" ephemeral type.
const EphemeralTyping TypingKind = "typing"

// UnmarshalJSON accepts only "typing": the ephemeral list is strict, and a
// value the kit does not know invalidates the whole batch.
func (k *TypingKind) UnmarshalJSON(b []byte) error {
	var s string
	if err := json.Unmarshal(b, &s); err != nil {
		return err
	}
	if TypingKind(s) != EphemeralTyping {
		return fmt.Errorf("unknown ephemeral type %q", s)
	}
	*k = EphemeralTyping
	return nil
}

// Typing asks the adapter to show a typing indicator in a conversation.
type Typing struct {
	Type         TypingKind `json:"type"`
	Conversation string     `json:"conversation"`
}

type typingRaw struct {
	Type         *TypingKind `json:"type" kit:"required"`
	Conversation *string     `json:"conversation" kit:"required"`
}

func (t *typingRaw) validate(at string) *Error {
	return checkRef(*t.Conversation, at+"/conversation")
}

// outboxResponseRaw is the wire form of the outbox answer. The actions stay
// raw so that one unreadable action does not take the others down.
type outboxResponseRaw struct {
	Actions   []json.RawMessage `json:"actions" kit:"required"`
	Ephemeral []typingRaw       `json:"ephemeral" kit:"required"`
}

// Validate checks every ephemeral entry.
func (r *outboxResponseRaw) Validate() *Error {
	for i := range r.Ephemeral {
		if err := r.Ephemeral[i].validate(fmt.Sprintf("/ephemeral/%d", i)); err != nil {
			return err
		}
	}
	return nil
}

// OutboxResponse is the core's answer to an outbox poll. Each action is decoded
// on its own (ParseAction or DecodeAction); Ephemeral is strict.
type OutboxResponse struct {
	Actions   []json.RawMessage `json:"actions"`
	Ephemeral []Typing          `json:"ephemeral"`
}

// DecodeOutboxResponse decodes the batch envelope. Both fields are required,
// even though the shape is otherwise lenient.
func DecodeOutboxResponse(b []byte) (OutboxResponse, error) {
	r, err := DecodeLenient[outboxResponseRaw](b)
	if err != nil {
		return OutboxResponse{}, err
	}
	out := OutboxResponse{Actions: r.Actions, Ephemeral: make([]Typing, len(r.Ephemeral))}
	for i, t := range r.Ephemeral {
		out.Ephemeral[i] = Typing{Type: *t.Type, Conversation: *t.Conversation}
	}
	return out, nil
}

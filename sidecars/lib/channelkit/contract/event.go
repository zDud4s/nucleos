package contract

import (
	"encoding/json"
	"fmt"
	"time"
)

// ConversationKind is the kind of an inbound conversation.
type ConversationKind string

const (
	ConvPrivate ConversationKind = "private"
	ConvGroup   ConversationKind = "group"
	ConvThread  ConversationKind = "thread"
)

// UnmarshalJSON accepts only the known kinds; anything else is a decode error.
func (k *ConversationKind) UnmarshalJSON(b []byte) error {
	var s string
	if err := json.Unmarshal(b, &s); err != nil {
		return err
	}
	switch ConversationKind(s) {
	case ConvPrivate, ConvGroup, ConvThread:
		*k = ConversationKind(s)
		return nil
	}
	return fmt.Errorf("unknown conversation kind %q", s)
}

// PartType discriminates an inbound message part (T3).
type PartType string

const (
	PartText        PartType = "text"
	PartAudio       PartType = "audio"
	PartImage       PartType = "image"
	PartFile        PartType = "file"
	PartUnsupported PartType = "unsupported"
)

// UnmarshalJSON accepts only the known part types; an adapter must send
// "unsupported" for what it cannot map, so anything else is a decode error.
func (t *PartType) UnmarshalJSON(b []byte) error {
	var s string
	if err := json.Unmarshal(b, &s); err != nil {
		return err
	}
	switch PartType(s) {
	case PartText, PartAudio, PartImage, PartFile, PartUnsupported:
		*t = PartType(s)
		return nil
	}
	return fmt.Errorf("unknown part type %q", s)
}

// Event types handled by Event.Validate.
const (
	eventTypeMessage = "message"
	eventTypeAction  = "action"
)

// Conversation is where an event happened; Parent is set iff Kind is thread.
type Conversation struct {
	Ref    *string           `json:"ref" kit:"required"`
	Kind   *ConversationKind `json:"kind" kit:"required"`
	Parent *string           `json:"parent,omitempty"`
}

// Sender is who produced an event.
type Sender struct {
	Ref  *string `json:"ref" kit:"required"`
	Name *string `json:"name,omitempty"`
}

// InPart is one part of an inbound message. It is a raw struct (T12): every
// field any type may carry is optional here and Validate enforces the per-type
// rules.
type InPart struct {
	Type       *PartType `json:"type" kit:"required"`
	Text       *string   `json:"text,omitempty"`
	Media      *string   `json:"media,omitempty"`
	Mime       *string   `json:"mime,omitempty"`
	DurationMS *uint64   `json:"duration_ms,omitempty"`
	Caption    *string   `json:"caption,omitempty"`
	Name       *string   `json:"name,omitempty"`
	Label      *string   `json:"label,omitempty"`
}

// Message is the payload of a type:message event.
type Message struct {
	Ref     *string  `json:"ref" kit:"required"`
	ReplyTo *string  `json:"reply_to,omitempty"`
	Parts   []InPart `json:"parts" kit:"required"`
}

// Event is an inbound event (an input shape: decode it strictly). At is kept as
// the string that arrived (T5). The origin never travels in the body.
type Event struct {
	EventID      *string        `json:"event_id" kit:"required"`
	At           *string        `json:"at,omitempty"`
	Conversation *Conversation  `json:"conversation" kit:"required"`
	Sender       *Sender        `json:"sender" kit:"required"`
	Type         *string        `json:"type" kit:"required"`
	Message      *Message       `json:"message,omitempty"`
	Action       *ActionPayload `json:"action,omitempty"`
}

// ActionPayload is the payload of a type:action event: a button press. Token is
// the opaque button token the adapter received from the core.
type ActionPayload struct {
	Ref        *string `json:"ref" kit:"required"`
	MessageRef *string `json:"message_ref" kit:"required"`
	Token      *string `json:"token" kit:"required"`
}

// Validate runs the second stage: grammars, per-type rules and the type-specific
// payload.
func (e *Event) Validate() *Error {
	if *e.EventID == "" {
		return errEmpty("/event_id")
	}
	if err := checkRef(*e.EventID, "/event_id"); err != nil {
		return err
	}
	if e.At != nil {
		if _, err := time.Parse(time.RFC3339, *e.At); err != nil {
			return errGrammar("/at")
		}
	}
	if err := e.Conversation.validate("/conversation"); err != nil {
		return err
	}
	if err := e.Sender.validate("/sender"); err != nil {
		return err
	}
	switch *e.Type {
	case eventTypeMessage:
		return e.validateMessage()
	case eventTypeAction:
		return e.validateAction()
	default:
		return errOneOf("/type")
	}
}

func (c *Conversation) validate(at string) *Error {
	if err := checkRef(*c.Ref, at+"/ref"); err != nil {
		return err
	}
	if *c.Kind == ConvThread {
		if c.Parent == nil {
			return errRequiredIf(at + "/parent")
		}
		return checkRef(*c.Parent, at+"/parent")
	}
	if c.Parent != nil {
		return errForbidden(at + "/parent")
	}
	return nil
}

func (s *Sender) validate(at string) *Error {
	if err := checkRef(*s.Ref, at+"/ref"); err != nil {
		return err
	}
	if s.Name != nil {
		return checkRef(*s.Name, at+"/name")
	}
	return nil
}

func (e *Event) validateMessage() *Error {
	// A message event carries no payload of another type.
	if e.Action != nil {
		return errForbidden("/action")
	}
	m := e.Message
	if m == nil {
		return errRequiredIf("/message")
	}
	if err := checkRef(*m.Ref, "/message/ref"); err != nil {
		return err
	}
	if m.ReplyTo != nil {
		if err := checkRef(*m.ReplyTo, "/message/reply_to"); err != nil {
			return err
		}
	}
	if len(m.Parts) == 0 {
		return errEmpty("/message/parts")
	}
	for i := range m.Parts {
		if err := m.Parts[i].validate(fmt.Sprintf("/message/parts/%d", i)); err != nil {
			return err
		}
	}
	return nil
}

func (e *Event) validateAction() *Error {
	// An action event carries no message.
	if e.Message != nil {
		return errForbidden("/message")
	}
	a := e.Action
	if a == nil {
		return errRequiredIf("/action")
	}
	if err := checkRef(*a.Ref, "/action/ref"); err != nil {
		return err
	}
	if err := checkRef(*a.MessageRef, "/action/message_ref"); err != nil {
		return err
	}
	return checkAdapterToken(*a.Token, "/action/token")
}

// validate applies the per-type rules of an inbound part.
func (p *InPart) validate(at string) *Error {
	forbid := func(fields ...struct {
		name string
		set  bool
	}) *Error {
		for _, f := range fields {
			if f.set {
				return errForbidden(at + "/" + f.name)
			}
		}
		return nil
	}
	type fld = struct {
		name string
		set  bool
	}
	media := fld{"media", p.Media != nil}
	mime := fld{"mime", p.Mime != nil}
	dur := fld{"duration_ms", p.DurationMS != nil}
	caption := fld{"caption", p.Caption != nil}
	name := fld{"name", p.Name != nil}
	label := fld{"label", p.Label != nil}
	text := fld{"text", p.Text != nil}

	switch *p.Type {
	case PartText:
		if p.Text == nil {
			return errRequiredIf(at + "/text")
		}
		if *p.Text == "" {
			return errEmpty(at + "/text")
		}
		return forbid(media, mime, dur, caption, name, label)
	case PartUnsupported:
		if p.Label == nil {
			return errRequiredIf(at + "/label")
		}
		if !labelRe.MatchString(*p.Label) {
			return errGrammar(at + "/label")
		}
		return forbid(text, media, mime, dur, caption, name)
	}
	// audio, image and file reference a media upload.
	if err := forbid(text, label); err != nil {
		return err
	}
	if p.Media == nil {
		return errRequiredIf(at + "/media")
	}
	if !mediaIDRe.MatchString(*p.Media) {
		return errGrammar(at + "/media")
	}
	if p.Mime != nil && !mimeRe.MatchString(*p.Mime) {
		return errGrammar(at + "/mime")
	}
	if *p.Type != PartAudio {
		if err := forbid(dur); err != nil {
			return err
		}
	}
	if *p.Type == PartFile {
		if p.Name == nil {
			return errRequiredIf(at + "/name")
		}
		return checkRef(*p.Name, at+"/name")
	}
	return forbid(name)
}

package contract

import (
	"encoding/json"
	"fmt"
	"strconv"
)

// ActionType discriminates an outbox action (T3).
type ActionType string

const (
	ActionSend         ActionType = "send"
	ActionEdit         ActionType = "edit"
	ActionAnswerAction ActionType = "answer_action"
)

// UnmarshalJSON accepts only the known action types; any other is a decode error.
func (t *ActionType) UnmarshalJSON(b []byte) error {
	var s string
	if err := json.Unmarshal(b, &s); err != nil {
		return err
	}
	switch ActionType(s) {
	case ActionSend, ActionEdit, ActionAnswerAction:
		*t = ActionType(s)
		return nil
	}
	return fmt.Errorf("unknown action type %q", s)
}

// OutPartType discriminates an outbound message part. There is no "unsupported"
// on the way out: that part exists only on input.
type OutPartType string

const (
	OutText  OutPartType = "text"
	OutImage OutPartType = "image"
	OutFile  OutPartType = "file"
	OutAudio OutPartType = "audio"
)

// UnmarshalJSON accepts only the four outbound part types.
func (t *OutPartType) UnmarshalJSON(b []byte) error {
	var s string
	if err := json.Unmarshal(b, &s); err != nil {
		return err
	}
	switch OutPartType(s) {
	case OutText, OutImage, OutFile, OutAudio:
		*t = OutPartType(s)
		return nil
	}
	return fmt.Errorf("unknown outbound part type %q", s)
}

// outPartRaw is the wire form of an outbound part: every field any type may
// carry, optional; validate enforces the per-type rules.
type outPartRaw struct {
	Type    *OutPartType `json:"type" kit:"required"`
	Text    *string      `json:"text,omitempty"`
	Media   *string      `json:"media,omitempty"`
	Caption *string      `json:"caption,omitempty"`
	Name    *string      `json:"name,omitempty"`
	Mime    *string      `json:"mime,omitempty"`
}

// buttonRaw is the wire form of one inline button.
type buttonRaw struct {
	Label *string `json:"label" kit:"required"`
	Token *string `json:"token" kit:"required"`
}

// actionRaw is the wire form of an outbox action: the fields of every action type,
// optional. Validate dispatches on Type. The edit and answer_action fields join it
// in chunk B13b.
type actionRaw struct {
	ID                 *uint64       `json:"id" kit:"required"`
	Type               *ActionType   `json:"type" kit:"required"`
	Conversation       *string       `json:"conversation" kit:"required"`
	ReplyTo            *uint64       `json:"reply_to,omitempty"`
	ReplyToPlatformRef *string       `json:"reply_to_platform_ref,omitempty"`
	Parts              []outPartRaw  `json:"parts,omitempty" kit:"required"`
	Buttons            [][]buttonRaw `json:"buttons,omitempty"`
	Silent             bool          `json:"silent,omitempty"`
}

// Validate checks the fields common to all actions, then the rules of the type.
// edit and answer_action are not decoded yet (chunk B13b).
func (a *actionRaw) Validate() *Error {
	if *a.ID == 0 {
		return errRange("/id")
	}
	if err := checkRef(*a.Conversation, "/conversation"); err != nil {
		return err
	}
	switch *a.Type {
	case ActionSend:
		return a.validateSend()
	}
	return &Error{Code: CodeDecode, At: "/type"}
}

func (a *actionRaw) validateSend() *Error {
	if a.ReplyTo != nil && *a.ReplyTo == 0 {
		return errRange("/reply_to")
	}
	if a.ReplyToPlatformRef != nil {
		if a.ReplyTo == nil {
			return errForbidden("/reply_to_platform_ref")
		}
		if err := checkRef(*a.ReplyToPlatformRef, "/reply_to_platform_ref"); err != nil {
			return err
		}
	}
	if err := validateOutParts(a.Parts); err != nil {
		return err
	}
	return validateButtons(a.Buttons)
}

// validateOutParts requires at least one part and checks each of them.
func validateOutParts(parts []outPartRaw) *Error {
	if len(parts) == 0 {
		return errEmpty("/parts")
	}
	for i := range parts {
		if err := parts[i].validate("/parts/" + strconv.Itoa(i)); err != nil {
			return err
		}
	}
	return nil
}

// validate applies the per-type rules of an outbound part.
func (p *outPartRaw) validate(at string) *Error {
	forbid := func(name string, set bool) *Error {
		if set {
			return errForbidden(at + "/" + name)
		}
		return nil
	}
	if *p.Type == OutText {
		if p.Text == nil {
			return errRequiredIf(at + "/text")
		}
		if *p.Text == "" {
			return errEmpty(at + "/text")
		}
		for _, f := range []struct {
			name string
			set  bool
		}{{"media", p.Media != nil}, {"mime", p.Mime != nil}, {"caption", p.Caption != nil}, {"name", p.Name != nil}} {
			if err := forbid(f.name, f.set); err != nil {
				return err
			}
		}
		return nil
	}
	// image, file and audio reference a media upload.
	if err := forbid("text", p.Text != nil); err != nil {
		return err
	}
	if p.Media == nil {
		return errRequiredIf(at + "/media")
	}
	if !mediaIDRe.MatchString(*p.Media) {
		return errGrammar(at + "/media")
	}
	if *p.Type == OutImage {
		if err := forbid("mime", p.Mime != nil); err != nil {
			return err
		}
	} else if p.Mime != nil && !mimeRe.MatchString(*p.Mime) {
		return errGrammar(at + "/mime")
	}
	if p.Caption != nil {
		if err := checkRef(*p.Caption, at+"/caption"); err != nil {
			return err
		}
	}
	if *p.Type == OutFile {
		if p.Name == nil {
			return errRequiredIf(at + "/name")
		}
		return checkRef(*p.Name, at+"/name")
	}
	return forbid("name", p.Name != nil)
}

// validateButtons checks the rows of inline buttons: no empty row, a 1-256 byte
// label and a token in the core's grammar.
func validateButtons(rows [][]buttonRaw) *Error {
	for i, row := range rows {
		at := "/buttons/" + strconv.Itoa(i)
		if len(row) == 0 {
			return errEmpty(at)
		}
		for j, b := range row {
			bat := at + "/" + strconv.Itoa(j)
			if *b.Label == "" {
				return errEmpty(bat + "/label")
			}
			if err := checkRef(*b.Label, bat+"/label"); err != nil {
				return err
			}
			if !tokenRe.MatchString(*b.Token) {
				return errGrammar(bat + "/token")
			}
		}
	}
	return nil
}

// OutPart is one part of an outbound message. Only the fields of its Type are set.
type OutPart struct {
	Type    OutPartType `json:"type"`
	Text    string      `json:"text,omitempty"`
	Media   string      `json:"media,omitempty"`
	Caption string      `json:"caption,omitempty"`
	Name    string      `json:"name,omitempty"`
	Mime    string      `json:"mime,omitempty"`
}

// Button is one inline button; Token is the opaque value the core issued.
type Button struct {
	Label string `json:"label"`
	Token string `json:"token"`
}

// Action is one outbox action: Send now, Edit and AnswerAction in chunk B13b.
type Action interface {
	// ActionID is the id of the outbox row the action came from (always >= 1).
	ActionID() uint64
	// Type is the wire discriminator of the action.
	Type() ActionType
}

// Send posts a new message to a conversation.
type Send struct {
	ID                 uint64
	Conversation       string
	ReplyTo            *uint64
	ReplyToPlatformRef string
	Parts              []OutPart
	Buttons            [][]Button
	Silent             bool
}

// ActionID returns the outbox row id.
func (s Send) ActionID() uint64 { return s.ID }

// Type returns ActionSend.
func (Send) Type() ActionType { return ActionSend }

type sendWire struct {
	ID                 uint64     `json:"id"`
	Type               ActionType `json:"type"`
	Conversation       string     `json:"conversation"`
	ReplyTo            *uint64    `json:"reply_to,omitempty"`
	ReplyToPlatformRef string     `json:"reply_to_platform_ref,omitempty"`
	Parts              []OutPart  `json:"parts"`
	Buttons            [][]Button `json:"buttons,omitempty"`
	Silent             bool       `json:"silent,omitempty"`
}

// MarshalJSON encodes the action in its wire form, omitting defaults (T6).
func (s Send) MarshalJSON() ([]byte, error) {
	return json.Marshal(sendWire{s.ID, ActionSend, s.Conversation, s.ReplyTo, s.ReplyToPlatformRef, s.Parts, s.Buttons, s.Silent})
}

func convertParts(raw []outPartRaw) []OutPart {
	out := make([]OutPart, len(raw))
	for i, p := range raw {
		o := OutPart{Type: *p.Type}
		if p.Text != nil {
			o.Text = *p.Text
		}
		if p.Media != nil {
			o.Media = *p.Media
		}
		if p.Caption != nil {
			o.Caption = *p.Caption
		}
		if p.Name != nil {
			o.Name = *p.Name
		}
		if p.Mime != nil {
			o.Mime = *p.Mime
		}
		out[i] = o
	}
	return out
}

func convertButtons(raw [][]buttonRaw) [][]Button {
	if len(raw) == 0 {
		return nil
	}
	out := make([][]Button, len(raw))
	for i, row := range raw {
		out[i] = make([]Button, len(row))
		for j, b := range row {
			out[i][j] = Button{Label: *b.Label, Token: *b.Token}
		}
	}
	return out
}

// ParseAction decodes one outbox action leniently (an output shape: unknown keys
// are ignored). Only send is implemented; edit, answer_action and unknown types
// return a decode error until chunk B13b adds them. B13b wraps this in DecodeAction,
// which also builds the rescue ack.
func ParseAction(b []byte) (Action, error) {
	r, err := DecodeLenient[actionRaw](b)
	if err != nil {
		return nil, err
	}
	// Validate has accepted only ActionSend.
	s := Send{
		ID:           *r.ID,
		Conversation: *r.Conversation,
		ReplyTo:      r.ReplyTo,
		Parts:        convertParts(r.Parts),
		Buttons:      convertButtons(r.Buttons),
		Silent:       r.Silent,
	}
	if r.ReplyToPlatformRef != nil {
		s.ReplyToPlatformRef = *r.ReplyToPlatformRef
	}
	return s, nil
}

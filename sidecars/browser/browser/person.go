// §spec browser-volante

package browser

import (
	"context"
	"encoding/json"
	"errors"
)

// ErrNotSoleSession refuses a person's turn in a browser that holds more than one session.
//
// The fence is browser-wide, so lifting it for one session lifts it for every other session the agent
// has open. A person may only drive when theirs is the only one.
var ErrNotSoleSession = errors.New("browser: a person can only drive a browser that has exactly one session")

// ErrNotPerson is EndPerson for a session no person holds.
var ErrNotPerson = errors.New("browser: this session is not held by a person")

// ConsequencePersonDriving is the refusal an agent's act gets while a person drives the session. It is
// not ConsequenceWheelRequested: that one says the wheel was asked for, this one says it was taken.
const ConsequencePersonDriving Consequence = "person-driving"

// PersonSeat is the person's turn in the agent's OWN browser, with the fence lifted for as long as it
// lasts. BeginPerson swaps the fence for the person; EndPerson restores it, and only after the pages
// were reloaded and the profile swept of workers again. The chain it returns is what the person's own
// navigation produced.
type PersonSeat interface {
	BeginPerson(ctx context.Context, id SessionID) error
	EndPerson(ctx context.Context, id SessionID) (Returned, error)
}

// ErrBadEvent refuses an input batch that holds an event of a kind or type the contract does not name.
var ErrBadEvent = errors.New("browser: unknown input event")

// InputEvent is one thing a person did: a pointer move or click, a wheel turn, a key, or a run of text.
type InputEvent struct {
	Kind       string  `json:"kind"`
	Type       string  `json:"type"`
	X          float64 `json:"x"`
	Y          float64 `json:"y"`
	Button     string  `json:"button"`
	Buttons    int     `json:"buttons"`
	ClickCount int     `json:"clickCount"`
	Modifiers  int     `json:"modifiers"`
	DX         float64 `json:"dx"`
	DY         float64 `json:"dy"`
	Key        string  `json:"key"`
	Code       string  `json:"code"`
	// KeyCode is the key's Windows virtual key code, which Chrome needs to apply an editing key such as
	// Backspace or Enter. Zero (or omitted) means the sender did not name one.
	KeyCode int    `json:"keyCode"`
	Text    string `json:"text"`
	Value   string `json:"value"`
}

// PersonInput applies a person's input batch to their page, in order. Refused with ErrNotPerson unless
// the person holds the session.
type PersonInput interface {
	Input(ctx context.Context, id SessionID, events []InputEvent) error
}

// ErrNoPrompt answers a prompt nobody raised, or one that was already answered, timed out or cancelled.
var ErrNoPrompt = errors.New("browser: no such pending prompt")

// ErrBadAnswer refuses an answer that does not fit the prompt it names.
var ErrBadAnswer = errors.New("browser: the answer does not fit the prompt")

// Prompt is something a page asked a person: a JS dialog, an HTTP credential, a select, a file. It is
// streamed to every viewer as a P record. A Prompt with Resolved set is the record that closes the one
// of the same ID, and carries nothing else.
//
// Multiple is a pointer so that false is still written for a select or a file, where it means "one".
type Prompt struct {
	ID            string         `json:"id"`
	Kind          string         `json:"kind"`
	Resolved      bool           `json:"resolved,omitempty"`
	DialogType    string         `json:"dialogType,omitempty"`
	Message       string         `json:"message,omitempty"`
	DefaultPrompt string         `json:"defaultPrompt,omitempty"`
	Options       []PromptOption `json:"options,omitempty"`
	Multiple      *bool          `json:"multiple,omitempty"`
	Accept        string         `json:"accept,omitempty"`
	Origin        string         `json:"origin,omitempty"`
	Realm         string         `json:"realm,omitempty"`
}

// PromptOption is one choice of a select prompt.
type PromptOption struct {
	Value    string `json:"value"`
	Label    string `json:"label"`
	Selected bool   `json:"selected"`
}

// PersonAnswer delivers a person's answer to a pending prompt. The answer's shape depends on the
// prompt's kind and is validated by the driver. Refused with ErrNotPerson unless the person holds the
// session, then with ErrNoPrompt for an unknown prompt.
type PersonAnswer interface {
	Answer(ctx context.Context, id SessionID, prompt string, answer json.RawMessage) error
}

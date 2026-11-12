// §spec browser-com-painel

package browser

import (
	"context"
	"encoding/json"
)

// PanelEnd says why a panel's channel ended.
type PanelEnd string

const (
	// PanelPersonClosed is the person closing the window the panel lives in.
	PanelPersonClosed PanelEnd = "person-closed"
	// PanelSessionClosed is the session being closed from this side.
	PanelSessionClosed PanelEnd = "closed"
)

// PanelClosed is the error a panel channel ends with when the session ended under it, rather than
// the listener leaving.
type PanelClosed struct{ Reason PanelEnd }

func (e PanelClosed) Error() string { return "browser: panel closed: " + string(e.Reason) }

// Panel is the optional ability to talk to the panel a visible session carries.
//
// It sits outside Driver for the reason Watcher does: the panel is for a person, not an agent verb,
// so it must not grow the surface the agent is given. PanelPush says something to the panel and keeps
// it, so a panel that is reloaded or navigated opens onto the conversation as it stood. PanelEvents
// delivers what the panel says until ctx ends or the session does; the sink may block, and the
// implementation must never let a blocked sink stall the browser.
type Panel interface {
	PanelPush(ctx context.Context, id SessionID, msg json.RawMessage) error
	PanelEvents(ctx context.Context, id SessionID, sink func(json.RawMessage)) error
}

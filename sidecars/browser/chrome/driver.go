// Package chrome implements browser.Driver over CDP.
//
// # The fence is a constructor, not a flag
//
// Spec §6.2a: "se o interceptor não estiver atado, o separador não navega." The strongest way to say
// that in Go is to make an unfenced Driver impossible to hold. [Connect] attaches the interception
// and returns an error if it cannot; there is no exported way to build a Driver otherwise. So there
// is no `if !d.fenced` to forget, and no window between construction and arming.
//
// The interception goes on the BROWSER session (see cdp.SessionID). The spike measured that a
// page-session fence never sees a service worker's script fetch at all.
package chrome

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// Driver drives one browser. Holding one means the fence is attached.
type Driver struct {
	conn *cdp.Conn

	mu       sync.Mutex
	sessions map[browser.SessionID]*session
	counter  int
	// targets maps a CDP target to the session that owns it, so a popup can be traced back to the
	// session whose page opened it.
	targets map[string]browser.SessionID
}

type session struct {
	id        browser.SessionID
	target    string
	cdp       cdp.SessionID
	mode      browser.Mode
	requested string
	final     string
	title     string
	// refs maps a snapshot ref ("e5") to the node it named. The agent may only act on something a
	// snapshot actually showed it — see Act.
	refs map[string]int64
}

// Connect attaches the fence and returns a Driver.
//
// The order below is the security argument, and it is asserted by a test rather than trusted:
// interception first, auto-attach second, and no target is created by this function at all. A
// Driver that returned successfully without both calls landing would be an unfenced browser wearing
// the type that promises otherwise.
func Connect(ctx context.Context, conn *cdp.Conn) (*Driver, error) {
	if _, err := conn.Call(ctx, cdp.BrowserSession, "Fetch.enable", map[string]any{
		"patterns": []map[string]any{{"urlPattern": "*"}},
	}); err != nil {
		return nil, fmt.Errorf("%w: %v", browser.ErrFenceNotAttached, err)
	}

	// waitForDebuggerOnStart is what closes the TOCTOU window of spec §5.4: a new target is born
	// paused, with an empty url, and the interception is already on before it may navigate.
	if _, err := conn.Call(ctx, cdp.BrowserSession, "Target.setAutoAttach", map[string]any{
		"autoAttach":             true,
		"waitForDebuggerOnStart": true,
		"flatten":                true,
	}); err != nil {
		return nil, fmt.Errorf("%w: %v", browser.ErrFenceNotAttached, err)
	}

	driver := &Driver{
		conn:     conn,
		sessions: map[browser.SessionID]*session{},
		targets:  map[string]browser.SessionID{},
	}
	conn.OnEvent(driver.onEvent)
	return driver, nil
}

func (d *Driver) Name() string { return "chrome" }

// onEvent services targets that are born paused.
//
// Auto-attach is HIERARCHICAL: a browser-level subscription is offered pages, but a page's own
// out-of-process iframes are only offered to whoever auto-attached on THAT page's session. The
// spike measured this — a cross-site iframe never appeared as a target until each session re-armed
// on its own children — so every attached target re-arms before it is released.
func (d *Driver) onEvent(event cdp.Event) {
	if event.Method != "Target.attachedToTarget" {
		return
	}
	var params struct {
		SessionID  cdp.SessionID `json:"sessionId"`
		TargetInfo struct {
			TargetID string `json:"targetId"`
			Type     string `json:"type"`
			OpenerID string `json:"openerId"`
			URL      string `json:"url"`
		} `json:"targetInfo"`
		WaitingForDebugger bool `json:"waitingForDebugger"`
	}
	if err := json.Unmarshal(event.Params, &params); err != nil {
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	// Re-arm on this target's own children before letting it run.
	_, _ = d.conn.Call(ctx, params.SessionID, "Target.setAutoAttach", map[string]any{
		"autoAttach":             true,
		"waitForDebuggerOnStart": true,
		"flatten":                true,
		"filter":                 []map[string]any{{}},
	})

	// A popup belongs to the session whose page opened it. Identified by openerId and never by
	// arrival order: the spike watched Chrome raise three page attaches for one window.open, and a
	// "first new target" heuristic bound the fence to the wrong one — silently, which is the worst
	// way for a fence to be wrong.
	if params.TargetInfo.OpenerID != "" {
		d.mu.Lock()
		if owner, ok := d.targets[params.TargetInfo.OpenerID]; ok {
			d.targets[params.TargetInfo.TargetID] = owner
		}
		d.mu.Unlock()
	}

	if params.WaitingForDebugger {
		_, _ = d.conn.Call(ctx, params.SessionID, "Runtime.runIfWaitingForDebugger", nil)
	}
}

// Open creates a target, arms it while it is still paused, and only then navigates.
func (d *Driver) Open(ctx context.Context, req browser.OpenRequest) (browser.Session, error) {
	attached := make(chan attachment, 4)
	cancel := d.conn.OnEvent(func(event cdp.Event) {
		if event.Method != "Target.attachedToTarget" {
			return
		}
		var params struct {
			SessionID  cdp.SessionID `json:"sessionId"`
			TargetInfo struct {
				TargetID string `json:"targetId"`
				Type     string `json:"type"`
			} `json:"targetInfo"`
		}
		if err := json.Unmarshal(event.Params, &params); err != nil {
			return
		}
		if params.TargetInfo.Type != "page" {
			return
		}
		select {
		case attached <- attachment{session: params.SessionID, target: params.TargetInfo.TargetID}:
		default:
		}
	})
	defer cancel()

	created, err := d.conn.Call(ctx, cdp.BrowserSession, "Target.createTarget", map[string]any{
		"url": "about:blank",
	})
	if err != nil {
		return browser.Session{}, fmt.Errorf("creating target: %w", err)
	}
	var target struct {
		TargetID string `json:"targetId"`
	}
	if err := json.Unmarshal(created, &target); err != nil {
		return browser.Session{}, err
	}

	cdpSession, err := d.sessionFor(ctx, target.TargetID, attached)
	if err != nil {
		return browser.Session{}, err
	}

	if _, err := d.conn.Call(ctx, cdpSession, "Page.enable", nil); err != nil {
		return browser.Session{}, fmt.Errorf("enabling page domain: %w", err)
	}

	d.mu.Lock()
	d.counter++
	id := browser.SessionID(fmt.Sprintf("s%d", d.counter))
	entry := &session{
		id:        id,
		target:    target.TargetID,
		cdp:       cdpSession,
		mode:      browser.ModeAgent,
		requested: req.URL,
		refs:      map[string]int64{},
	}
	d.sessions[id] = entry
	d.targets[target.TargetID] = id
	d.mu.Unlock()

	if _, err := d.conn.Call(ctx, cdpSession, "Page.navigate", map[string]any{"url": req.URL}); err != nil {
		return browser.Session{}, fmt.Errorf("navigating: %w", err)
	}

	entry.final = req.URL
	return browser.Session{
		ID:           id,
		Mode:         entry.mode,
		RequestedURL: entry.requested,
		FinalURL:     entry.final,
	}, nil
}

type attachment struct {
	session cdp.SessionID
	target  string
}

// sessionFor waits for the auto-attach of a target we just created. It does NOT call
// Target.attachToTarget: auto-attach is already on from Connect, and attaching a second time would
// produce a second session for the same target, which is how a fence ends up armed on one of them.
func (d *Driver) sessionFor(ctx context.Context, targetID string, attached <-chan attachment) (cdp.SessionID, error) {
	deadline := time.After(20 * time.Second)
	for {
		select {
		case got := <-attached:
			if got.target == targetID {
				return got.session, nil
			}
		case <-deadline:
			return "", errors.New("chrome: the target never attached")
		case <-ctx.Done():
			return "", ctx.Err()
		}
	}
}

func (d *Driver) lookup(id browser.SessionID) (*session, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	entry, ok := d.sessions[id]
	if !ok {
		return nil, browser.ErrNoSuchSession
	}
	return entry, nil
}

// Screenshot returns PNG bytes.
func (d *Driver) Screenshot(ctx context.Context, id browser.SessionID) ([]byte, error) {
	entry, err := d.lookup(id)
	if err != nil {
		return nil, err
	}
	result, err := d.conn.Call(ctx, entry.cdp, "Page.captureScreenshot", map[string]any{"format": "png"})
	if err != nil {
		return nil, err
	}
	var payload struct {
		Data string `json:"data"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		return nil, err
	}
	return base64.StdEncoding.DecodeString(payload.Data)
}

// Handoff marks the session as the person's.
//
// The relaunch itself belongs to the núcleo, not here: spec §4.2 says the process is discarded and
// a headful one is started over the SAME profile, and this driver does not know where profiles live
// — deliberately, since that is the boundary the whole pillar holds. What this does is close the
// agent's session gracefully, because the spike measured that a hard kill loses the last writes to
// the profile, and the profile is the identity the person is about to use.
func (d *Driver) Handoff(ctx context.Context, id browser.SessionID, reason string) (browser.HandoffTicket, error) {
	entry, err := d.lookup(id)
	if err != nil {
		return browser.HandoffTicket{}, err
	}
	entry.mode = browser.ModeHuman
	return browser.HandoffTicket{
		SessionID: id,
		Mode:      browser.ModeHuman,
		URL:       entry.final,
		Reason:    reason,
	}, nil
}

// Close ends one session's target.
func (d *Driver) Close(ctx context.Context, id browser.SessionID) error {
	entry, err := d.lookup(id)
	if err != nil {
		return err
	}
	_, callErr := d.conn.Call(ctx, cdp.BrowserSession, "Target.closeTarget", map[string]any{
		"targetId": entry.target,
	})
	d.mu.Lock()
	delete(d.sessions, id)
	delete(d.targets, entry.target)
	d.mu.Unlock()
	return callErr
}

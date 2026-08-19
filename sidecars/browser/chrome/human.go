package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"sync"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// Human drives the browser a person is looking at, which mostly means not driving it.
//
// # Why this is a different type and not a flag on Driver
//
// Driver's whole construction argument is that holding one means the fence is attached: Connect arms
// the interception, refuses if it cannot, and there is no other way to build one (spec §6.2a). A
// headful browser has no fence — spec §6.4 lifts the restrictions because the person is the one
// acting, and a fence would stop the login the handover exists for. Expressing that as a boolean on
// Driver would mean the type no longer promised anything, and the promise is load-bearing for every
// caller that never checks. So the unfenced case gets its own name, and the name says why.
//
// # What it does instead
//
// It records. Spec §5.3a: the hosts a person crosses DURING a login are the candidates that may be
// granted as a set when the wheel comes back, and nothing else in the system can see them — the
// núcleo only learns where the window ended up. This is the recorder, and it is the reason a headful
// browser is attached over CDP at all rather than simply launched and forgotten.
//
// # And it refuses the agent's verbs
//
// Snapshot, Act and Screenshot all fail with browser.ErrPersonIsDriving. The núcleo already refuses
// them from its own record (spec §4.4 rule 1), so this is the second layer — and for Screenshot it is
// the layer that matters most: the page in front of the person during a handover is a login form,
// with a password half-typed into it.
type Human struct {
	conn *cdp.Conn

	mu      sync.Mutex
	chain   []string
	id      browser.SessionID
	target  string
	cdp     cdp.SessionID
	url     string
	stopped bool

	unsubscribe func()
}

// ConnectHuman attaches to a headful browser and starts recording where the person goes.
//
// Discovery is at the BROWSER level and covers every target, popups included. That is not
// thoroughness for its own sake: an SSO login is very often a popup, spec §5.4 blocks popups in agent
// mode precisely because of what they can do, and a chain recorded only from the first tab would miss
// the identity provider — which is the one host §5.3a exists to capture.
func ConnectHuman(ctx context.Context, conn *cdp.Conn) (*Human, error) {
	human := &Human{conn: conn}
	human.unsubscribe = conn.OnEvent(human.onEvent)
	if _, err := conn.Call(ctx, cdp.BrowserSession, "Target.setDiscoverTargets", map[string]any{
		"discover": true,
	}); err != nil {
		human.unsubscribe()
		return nil, fmt.Errorf("chrome: cannot watch where the person goes: %w", err)
	}
	return human, nil
}

func (h *Human) Name() string { return "chrome-human" }

// onEvent records every url a page target reports.
//
// targetInfoChanged fires on each navigation, so the chain is the order the person actually moved in.
// Filtering to https happens later, in browser_policy's granted_origins, so that what is recorded
// here stays a faithful record of the navigation and the rules about what may be granted live in one
// place.
func (h *Human) onEvent(event cdp.Event) {
	switch event.Method {
	case "Target.targetCreated", "Target.targetInfoChanged":
	default:
		return
	}
	var params struct {
		TargetInfo struct {
			Type string `json:"type"`
			URL  string `json:"url"`
		} `json:"targetInfo"`
	}
	if err := json.Unmarshal(event.Params, &params); err != nil {
		return
	}
	if params.TargetInfo.Type != "page" || params.TargetInfo.URL == "" {
		return
	}
	h.record(params.TargetInfo.URL)
}

// record appends a url unless it is the one already at the end.
//
// Consecutive duplicates only. A chain that returns to where it started — jira, google, jira — is
// what a real login looks like, and collapsing that to a set here would take from `grant` the one
// piece of information it uses to tell the destination from the identity provider.
func (h *Human) record(url string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.stopped {
		return
	}
	if len(h.chain) > 0 && h.chain[len(h.chain)-1] == url {
		return
	}
	h.chain = append(h.chain, url)
	h.url = url
}

// Chain is what the person's navigation produced, in order.
func (h *Human) Chain() []string {
	h.mu.Lock()
	defer h.mu.Unlock()
	return append([]string(nil), h.chain...)
}

// Open shows the person the page, in a real window.
//
// Unlike the agent's Open there is no pause-arm-navigate dance, because there is nothing to arm. The
// target is created at the destination directly, which is also what makes the window appear at the
// page the person was asked about rather than at a blank tab that then moves.
func (h *Human) Open(ctx context.Context, req browser.OpenRequest) (browser.Session, error) {
	created, err := h.conn.Call(ctx, cdp.BrowserSession, "Target.createTarget", map[string]any{
		"url": req.URL,
	})
	if err != nil {
		return browser.Session{}, fmt.Errorf("opening the person's window: %w", err)
	}
	var target struct {
		TargetID string `json:"targetId"`
	}
	if err := json.Unmarshal(created, &target); err != nil {
		return browser.Session{}, err
	}

	attached, err := h.conn.Call(ctx, cdp.BrowserSession, "Target.attachToTarget", map[string]any{
		"targetId": target.TargetID,
		"flatten":  true,
	})
	if err != nil {
		return browser.Session{}, fmt.Errorf("attaching to the person's window: %w", err)
	}
	var session struct {
		SessionID cdp.SessionID `json:"sessionId"`
	}
	if err := json.Unmarshal(attached, &session); err != nil {
		return browser.Session{}, err
	}

	h.mu.Lock()
	h.id = browser.SessionID("h1")
	h.target = target.TargetID
	h.cdp = session.SessionID
	id := h.id
	h.mu.Unlock()

	// The requested url is recorded even if the target never reports it — the person may close the
	// window before it loads, and a chain that lost its own destination would grant the identity
	// provider on its own.
	h.record(req.URL)

	return browser.Session{
		ID:           id,
		Mode:         browser.ModeHuman,
		RequestedURL: req.URL,
		FinalURL:     req.URL,
	}, nil
}

// Close ends the person's window.
func (h *Human) Close(ctx context.Context, id browser.SessionID) error {
	h.mu.Lock()
	if h.id != id || h.target == "" {
		h.mu.Unlock()
		return browser.ErrNoSuchSession
	}
	target := h.target
	h.target = ""
	// Nothing after this point is part of the person's login, so the recorder stops here rather than
	// on the connection closing: the tabs Chrome touches while it shuts down are not somewhere anyone
	// chose to go, and they would arrive at `grant` looking exactly like somewhere they did.
	h.stopped = true
	h.mu.Unlock()

	closing, cancel := context.WithTimeout(context.WithoutCancel(ctx), 10*time.Second)
	defer cancel()
	_, err := h.conn.Call(closing, cdp.BrowserSession, "Target.closeTarget", map[string]any{
		"targetId": target,
	})
	return err
}

// Snapshot is refused: the page in front of the person is theirs.
func (h *Human) Snapshot(context.Context, browser.SessionID, browser.SnapshotRequest) (browser.Snapshot, error) {
	return browser.Snapshot{}, browser.ErrPersonIsDriving
}

// Act is refused. Spec §4.4 rule 1, at the last possible layer.
func (h *Human) Act(context.Context, browser.SessionID, browser.Action) (browser.ActResult, error) {
	return browser.ActResult{}, browser.ErrPersonIsDriving
}

// Screenshot is refused, and this is the refusal with the sharpest edge: the reason the wheel was
// handed over is almost always a login, so the pixels here are a password field with a person's
// fingers on it.
func (h *Human) Screenshot(context.Context, browser.SessionID) ([]byte, error) {
	return nil, browser.ErrPersonIsDriving
}

// Handoff is refused: the person already has it.
func (h *Human) Handoff(context.Context, browser.SessionID, string) (browser.HandoffTicket, error) {
	return browser.HandoffTicket{}, browser.ErrPersonIsDriving
}

// Detach stops the recorder. Called when the browser behind it is going away.
func (h *Human) Detach() {
	h.mu.Lock()
	h.stopped = true
	unsubscribe := h.unsubscribe
	h.unsubscribe = nil
	h.mu.Unlock()
	if unsubscribe != nil {
		unsubscribe()
	}
}

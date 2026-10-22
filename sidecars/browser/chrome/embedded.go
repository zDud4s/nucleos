// §spec pilar-de-browser

package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/fence"
)

// ErrEmbeddedOpen is what Open returns on an embedded driver. The browser is the person's, so the
// driver may only adopt the one tab the shell opened for it — see [Driver.Adopt].
var ErrEmbeddedOpen = errors.New("chrome: an embedded driver cannot open a target; adopt the shell's tab")

// ConnectEmbedded attaches the fence to a browser that is not ours (the person's WebView2) and
// returns a Driver that can adopt exactly one tab.
//
// It differs from [Connect] in what it leaves out, and each omission is a safety property:
//   - no browser-level Target.setAutoAttach: sibling targets of the WebView2 are never offered to
//     us, so they can never be paused, re-armed or released by us;
//   - no Target.createTarget, so no service-worker sweep scratch page either (see [Driver.Adopt]);
//   - the connection itself refuses Target.createTarget and every Storage.* command, so even a bug
//     elsewhere in this package cannot reach the person's cookies or open a window in their app.
//
// The interception and the download denial go on the browser session exactly as in Connect, and fail
// the same way: no Driver without them.
func ConnectEmbedded(ctx context.Context, conn *cdp.Conn, policy fence.Policy) (*Driver, error) {
	if err := policy.ValidateEmbedded(); err != nil {
		return nil, fmt.Errorf("%w: %v", browser.ErrFenceNotAttached, err)
	}

	// Before the first call, so there is no moment at which the connection could send either.
	conn.Refuse(func(method string) bool {
		return method == "Target.createTarget" || strings.HasPrefix(method, "Storage.")
	})

	if err := armBrowserFence(ctx, conn, false); err != nil {
		return nil, err
	}

	driver := newDriver(conn, policy)
	driver.embedded = true
	// Nothing to sweep: the sweep needs a scratch page, which this driver may not create. Closed so
	// that waitForSweep, were it ever reached, is not a wait forever.
	close(driver.swept)
	return driver, nil
}

// Adopt attaches to the tab the shell opened, fences it exactly like a headless session, and only then
// navigates it to url.
//
// The target is attached explicitly (flatten) rather than by auto-attach, and a page-level
// auto-attach is turned on afterwards so the tab's out-of-process iframes are offered to onEvent,
// which re-arms and releases them as it does for every target.
//
// There is no waitForSweep, unlike Open: the sweep needs a scratch page and an embedded driver may not
// create one. The cost is that a service worker the person's WebView2 profile already holds is not
// cleared; the interception still refuses any script fetch a NEW one would need.
func (d *Driver) Adopt(ctx context.Context, targetID, url string) (browser.Session, error) {
	if !d.embedded {
		return browser.Session{}, errors.New("chrome: Adopt is for an embedded driver")
	}

	attached, err := d.conn.Call(ctx, cdp.BrowserSession, "Target.attachToTarget", map[string]any{
		"targetId": targetID,
		"flatten":  true,
	})
	if err != nil {
		return browser.Session{}, fmt.Errorf("attaching to target: %w", err)
	}
	var attachedTo struct {
		SessionID cdp.SessionID `json:"sessionId"`
	}
	if err := json.Unmarshal(attached, &attachedTo); err != nil {
		return browser.Session{}, err
	}
	if attachedTo.SessionID == "" {
		return browser.Session{}, errors.New("chrome: attaching to target returned no session")
	}
	cdpSession := attachedTo.SessionID

	// The shell's tab is not ours to close: letting go of it before registration is a detach.
	detach := func(closing context.Context) {
		_, _ = d.conn.Call(closing, cdp.BrowserSession, "Target.detachFromTarget", map[string]any{
			"sessionId": string(cdpSession),
		})
	}

	// Fatal, as the interception is: a tab whose frames in other processes are never offered to us is
	// a tab with holes in the fence.
	if _, err := d.conn.Call(ctx, cdpSession, "Target.setAutoAttach", map[string]any{
		"autoAttach":             true,
		"waitForDebuggerOnStart": true,
		"flatten":                true,
	}); err != nil {
		closing, cancel := context.WithTimeout(context.Background(), fenceCallTimeout)
		defer cancel()
		detach(closing)
		return browser.Session{}, fmt.Errorf("%w: arming the tab's frames: %v", browser.ErrFenceNotAttached, err)
	}

	return d.arm(ctx, targetID, cdpSession, url, detach)
}

// OnEmbeddedGone registers fn to be called, off the driver's lock, with the id of an adopted session
// whose tab the person closed (the browser detached our CDP session without a [Driver.DetachEmbedded]).
func (d *Driver) OnEmbeddedGone(fn func(browser.SessionID)) {
	d.mu.Lock()
	d.onGone = fn
	d.mu.Unlock()
}

// DetachEmbedded lets go of the CDP session on the shell's tab without closing the tab or touching the
// fence: the browser-session interception stays on, the session id and its target id stay valid, and
// [Driver.ReattachEmbedded] picks the tab up again. A second detach is a no-op.
//
// From the detach on, the fence records the main-frame Document requests the person makes into a chain
// on the session; the recorder is installed before the CDP session is let go of, so none is missed.
//
// Only the wiring that dies with the CDP session is cleaned: the session-keyed maps, the execution
// contexts and a running screencast. A panel is left in place, since it is re-armed on reattach.
func (d *Driver) DetachEmbedded(ctx context.Context, id browser.SessionID) error {
	if !d.embedded {
		return errors.New("chrome: DetachEmbedded is for an embedded driver")
	}
	entry, err := d.lookup(id)
	if err != nil {
		return err
	}
	d.mu.Lock()
	if entry.detached {
		d.mu.Unlock()
		return nil
	}
	entry.detached = true
	entry.chain = &chainRecorder{}
	old := entry.cdp
	d.mu.Unlock()

	_, callErr := d.conn.Call(ctx, cdp.BrowserSession, "Target.detachFromTarget", map[string]any{
		"sessionId": string(old),
	})

	d.mu.Lock()
	d.forgetCDPWiring(id, entry)
	d.mu.Unlock()
	if callErr != nil {
		return fmt.Errorf("detaching from target: %w", callErr)
	}
	return nil
}

// forgetCDPWiring drops everything keyed to the CDP sessions a session owns. d.mu must be held.
func (d *Driver) forgetCDPWiring(id browser.SessionID, entry *session) {
	if cast, ok := d.casts[entry.cdp]; ok {
		cast.end()
		delete(d.casts, entry.cdp)
	}
	owned := map[cdp.SessionID]bool{entry.cdp: true}
	for on, owner := range d.cdpToSession {
		if owner == id {
			owned[on] = true
			delete(d.cdpToSession, on)
		}
	}
	for on := range entry.frames {
		owned[on] = true
	}
	for on := range owned {
		if on == entry.cdp {
			continue
		}
		if cast, ok := d.casts[on]; ok {
			cast.end()
			delete(d.casts, on)
		}
	}
	for key := range d.contexts {
		if owned[key.session] {
			delete(d.contexts, key)
		}
	}
	entry.frames = map[cdp.SessionID]frameRef{}
}

// ReattachEmbedded attaches again to a tab detached by [Driver.DetachEmbedded] and redoes the
// per-session wiring that [Driver.Adopt] and arm do, without navigating, without registering a new
// session id and without touching the browser-session fence. It is a no-op for an attached session.
// On a failure after the attach the new CDP session is let go again and the session stays detached.
//
// It returns the chain of main-frame documents the person's navigation produced while detached. The
// recording stops before the attach, so the agent's own first requests are not part of it; on an error
// the (stopped) chain stays on the session so a retry returns it. An attached session returns an empty,
// non-nil chain.
func (d *Driver) ReattachEmbedded(ctx context.Context, id browser.SessionID) (browser.Returned, error) {
	if !d.embedded {
		return browser.Returned{}, errors.New("chrome: ReattachEmbedded is for an embedded driver")
	}
	entry, err := d.lookup(id)
	if err != nil {
		return browser.Returned{}, err
	}
	d.mu.Lock()
	if !entry.detached {
		d.mu.Unlock()
		return browser.Returned{Chain: []string{}}, nil
	}
	target := entry.target
	visible := d.visible
	rec := entry.chain
	d.mu.Unlock()
	registered := false
	if rec != nil {
		rec.stop()
		// Any failure below leaves the session detached and the person still driving it, so the
		// recording must go on: a fresh recorder seeded with what the stopped one holds. Requests made
		// during the failed attempt itself are the agent's and stay out.
		defer func() {
			if registered {
				return
			}
			fresh := &chainRecorder{}
			for _, url := range rec.Chain() {
				fresh.record(url)
			}
			d.mu.Lock()
			if entry.detached && entry.chain == rec {
				entry.chain = fresh
			}
			d.mu.Unlock()
		}()
	}
	var chain []string

	attached, err := d.conn.Call(ctx, cdp.BrowserSession, "Target.attachToTarget", map[string]any{
		"targetId": target,
		"flatten":  true,
	})
	if err != nil {
		return browser.Returned{}, fmt.Errorf("attaching to target: %w", err)
	}
	var attachedTo struct {
		SessionID cdp.SessionID `json:"sessionId"`
	}
	if err := json.Unmarshal(attached, &attachedTo); err != nil {
		return browser.Returned{}, err
	}
	if attachedTo.SessionID == "" {
		return browser.Returned{}, errors.New("chrome: attaching to target returned no session")
	}
	cdpSession := attachedTo.SessionID

	defer func() {
		if registered {
			return
		}
		closing, cancel := context.WithTimeout(context.Background(), fenceCallTimeout)
		defer cancel()
		_, _ = d.conn.Call(closing, cdp.BrowserSession, "Target.detachFromTarget", map[string]any{
			"sessionId": string(cdpSession),
		})
	}()

	if _, err := d.conn.Call(ctx, cdpSession, "Target.setAutoAttach", map[string]any{
		"autoAttach":             true,
		"waitForDebuggerOnStart": true,
		"flatten":                true,
	}); err != nil {
		return browser.Returned{}, fmt.Errorf("%w: arming the tab's frames: %v", browser.ErrFenceNotAttached, err)
	}
	if _, err := d.conn.Call(ctx, cdpSession, "Page.enable", nil); err != nil {
		return browser.Returned{}, fmt.Errorf("enabling page domain: %w", err)
	}
	// The same wiring arm does, minus the navigation and the registration; duplicated rather than
	// factored out so arm stays untouched.
	_, _ = d.conn.Call(ctx, cdpSession, "Page.setLifecycleEventsEnabled", map[string]any{"enabled": true})
	d.watchCSP(ctx, cdpSession)
	d.armFerry(ctx, cdpSession)
	if visible {
		d.armPanel(ctx, cdpSession)
	}
	mainFrame := d.mainFrameOf(ctx, cdpSession)

	d.mu.Lock()
	if entry.detached {
		entry.cdp = cdpSession
		entry.detached = false
		entry.frameID = mainFrame
		d.cdpToSession[cdpSession] = id
		entry.frames = map[cdp.SessionID]frameRef{}
		// Refs from the old snapshot name nodes of a document we no longer see; mintedRefs is kept so a
		// ref is never reused.
		entry.refs = map[string]nodeKey{}
		entry.refByNode = map[nodeKey]string{}
		entry.lastReported = map[string]browser.Element{}
		if visible && entry.panel == nil {
			entry.panel = newPanelState()
		}
		registered = true
		if entry.chain != nil {
			chain = entry.chain.Chain()
			entry.chain = nil
		}
	}
	d.mu.Unlock()
	if registered && visible {
		d.adoptPanelWorlds(cdpSession)
	}
	return browser.Returned{Chain: append([]string{}, chain...)}, nil
}

// lookupAttached is lookup for the page verbs: a detached embedded session is the person's, and its
// stale CDP session must not be driven (or read) by the agent. Returns [browser.ErrPersonIsDriving].
func (d *Driver) lookupAttached(id browser.SessionID) (*session, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	entry, ok := d.sessions[id]
	if !ok {
		return nil, browser.ErrNoSuchSession
	}
	if entry.detached {
		return nil, browser.ErrPersonIsDriving
	}
	return entry, nil
}

// recordDetachedDocument writes down a main-frame Document request made while the embedded tab is
// detached (the person is driving it). It runs before the fence decides, so a refused host still lands
// in the chain the person's navigation produced.
func (d *Driver) recordDetachedDocument(paused fetchPaused) {
	if !d.embedded || !isDocumentType(paused.ResourceType) || paused.FrameID == "" {
		return
	}
	var rec *chainRecorder
	d.mu.Lock()
	for _, entry := range d.sessions {
		if entry.detached && entry.chain != nil && entry.frameID == paused.FrameID {
			rec = entry.chain
			break
		}
	}
	d.mu.Unlock()
	if rec != nil {
		rec.record(paused.Request.URL)
	}
}

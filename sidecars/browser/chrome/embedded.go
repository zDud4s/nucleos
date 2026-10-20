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
	if err := policy.Validate(); err != nil {
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

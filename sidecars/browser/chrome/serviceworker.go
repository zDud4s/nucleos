// §spec pilar-de-browser

package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// Service workers are the one thing a page can leave behind (spec §5.8).
//
// A registered worker runs after its page closes, can fetch in the background, and lives IN THE
// PROFILE — so it survives the headless→headful→headless cycle of §4.2 and comes back next time.
// That makes it the only hole in this pillar whose damage outlives the session that opened it.
//
// Two mechanisms, and they are not redundant. Refusing the script fetch (fence.Decide) stops a NEW
// registration. Sweeping stops an OLD one — something the person registered in their headful window,
// where there is no fence because they are the one acting. Neither covers the other's case.
//
// The sweep runs at open and not at close, deliberately: closing is where a crash loses the work,
// and the state that matters is what is in the profile when the AGENT starts, not what was there
// when it stopped.

// sweepWindow is how long the browser is given to report the registrations it already has.
//
// ServiceWorker.enable replays existing registrations as events rather than answering with a list,
// so there is no completion to wait for — only a window. Shortening it can make the sweep miss a
// registration; it cannot open the fence, because the fence against NEW workers is the refused
// script fetch and not this.
const sweepWindow = 500 * time.Millisecond

// startSweep clears the profile of registered workers, in the background.
//
// Background, with Open waiting on it, rather than inside Connect: the requirement is that nothing
// NAVIGATES before the profile is clean, which is where the risk actually is. Blocking Connect would
// buy nothing extra and would put half a second on the front of every browser that never opens a
// page.
func (d *Driver) startSweep() {
	go func() {
		defer close(d.swept)
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		d.sweepErr = d.sweepServiceWorkers(ctx)
	}()
}

// waitForSweep blocks until the profile is clean, and refuses if it could not be.
//
// The refusal is ErrFenceNotAttached and not a plain error, because that is what it is: spec §5.8
// makes the sweep part of the fence, and a browser that could not do it is a browser with somebody
// else's code running in the profile it is about to navigate.
func (d *Driver) waitForSweep(ctx context.Context) error {
	select {
	case <-d.swept:
	case <-ctx.Done():
		return ctx.Err()
	}
	if d.sweepErr != nil {
		return fmt.Errorf("%w: the profile could not be swept of service workers: %v",
			browser.ErrFenceNotAttached, d.sweepErr)
	}
	return nil
}

type workerRegistration struct {
	RegistrationID string `json:"registrationId"`
	ScopeURL       string `json:"scopeURL"`
	IsDeleted      bool   `json:"isDeleted"`
}

// sweepServiceWorkers clears the profile, from a scratch page.
//
// MEASURED, and it cost a gate run: ServiceWorker.enable does NOT exist on the browser session —
// real Chrome answers -32601, "wasn't found". The unit tests passed anyway because a fake browser
// answers everything, which is the failure mode gate/domains_test.go now exists to catch. So the
// sweep opens a blank page it owns, enables the domain there, and closes it again.
//
// Note the asymmetry with the rest of the fence, because it is easy to read as an inconsistency: the
// INTERCEPTION must be on the browser session (a page-session Fetch never sees a worker's script
// fetch at all), and the SWEEP must be on a page session (the domain is not there otherwise). Both
// are measurements, and they point in opposite directions.
func (d *Driver) sweepServiceWorkers(ctx context.Context) error {
	targetID, session, err := d.createPage(ctx)
	if err != nil {
		return fmt.Errorf("opening a page to sweep from: %w", err)
	}
	defer func() {
		_, _ = d.conn.Call(ctx, cdp.BrowserSession, "Target.closeTarget", map[string]any{
			"targetId": targetID,
		})
	}()

	found := make(chan workerRegistration, 32)
	cancel := d.conn.OnEvent(func(event cdp.Event) {
		if event.Method != "ServiceWorker.workerRegistrationUpdated" {
			return
		}
		var params struct {
			Registrations []workerRegistration `json:"registrations"`
		}
		if err := json.Unmarshal(event.Params, &params); err != nil {
			return
		}
		for _, registration := range params.Registrations {
			if registration.IsDeleted || registration.ScopeURL == "" {
				continue
			}
			select {
			case found <- registration:
			default:
			}
		}
	})
	defer cancel()

	if _, err := d.conn.Call(ctx, session, "ServiceWorker.enable", nil); err != nil {
		return err
	}

	deadline := time.After(sweepWindow)
	scopes := map[string]struct{}{}
	for collecting := true; collecting; {
		select {
		case registration := <-found:
			scopes[registration.ScopeURL] = struct{}{}
		case <-deadline:
			collecting = false
		case <-ctx.Done():
			return ctx.Err()
		}
	}

	for scope := range scopes {
		if _, err := d.conn.Call(ctx, session, "ServiceWorker.unregister", map[string]any{
			"scopeURL": scope,
		}); err != nil {
			// One that will not go is a fence failure, not a warning. The alternative is a browser
			// that navigates with a stranger's code already running in the profile — and reports
			// success while doing it.
			return fmt.Errorf("unregistering %s: %w", scope, err)
		}
		d.recordSessionRefusal("", browser.ConsequenceServiceWorker,
			"a service worker left in this profile was unregistered before the agent started")
	}
	return nil
}

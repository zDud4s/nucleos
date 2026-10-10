// §spec pilar-de-browser

package pool

import (
	"context"
	"fmt"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/chrome"
	"nucleosbrowser/fence"
	"nucleosbrowser/launch"
)

// ChromeLauncher starts the pinned Chromium behind a fence, and is the only place in this repo where
// the four halves of that sentence are assembled: a proxy, a command line, a CDP connection and a
// driver.
//
// The order below is load-bearing and each step is fatal, for spec §6.2a's reason: a browser that is
// up while part of its fence is not is indistinguishable, from the outside, from a browser that is
// properly fenced.
type ChromeLauncher struct {
	// ExecutablePath is the pinned Chromium (launch.Install.ExecutablePath).
	ExecutablePath string
	// CacheMB caps the profile's disk cache. Zero takes launch.DefaultCacheMB.
	CacheMB int
	// StartTimeout bounds how long the browser has to report its debugging port.
	StartTimeout time.Duration
}

func (l ChromeLauncher) Name() string { return "chrome" }

// Launch starts one browser over one profile directory.
//
// # Why the process does not get the caller's context
//
// ctx here is the context of whatever asked for a session — an HTTP request that ends in seconds.
// launch.Start runs the browser under exec.CommandContext, so passing that context through would kill
// the browser the moment the request that opened it returned. The process gets a context of its own,
// cancelled by Shutdown; ctx bounds the parts that must not outlive the request: dialling and
// connecting.
func (l ChromeLauncher) Launch(ctx context.Context, dir string, policy fence.Policy) (Instance, error) {
	return l.launch(ctx, dir, policy, false)
}

// LaunchVisible is Launch with a window: the same proxy, the same fence, the same CDP policy, only
// without --headless=new (see launch.Options.Visible). The panel wiring that shows the window is
// not here yet.
func (l ChromeLauncher) LaunchVisible(ctx context.Context, dir string, policy fence.Policy) (Instance, error) {
	return l.launch(ctx, dir, policy, true)
}

func (l ChromeLauncher) launch(ctx context.Context, dir string, policy fence.Policy, visible bool) (Instance, error) {
	// First, and before a browser exists. NewProxy validates the policy, so a policy that does not
	// hold up is found while the only thing to clean up is nothing at all.
	proxy, err := fence.NewProxy(policy)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", browser.ErrFenceNotAttached, err)
	}

	lifetime, cancel := context.WithCancel(context.Background())
	instance := &chromeInstance{proxy: proxy, cancel: cancel}

	process, err := launch.Start(lifetime, launch.Options{
		ExecutablePath: l.ExecutablePath,
		ProfileDir:     dir,
		Mode:           browser.ModeAgent,
		ProxyAddr:      proxy.Addr(),
		CacheMB:        l.CacheMB,
		Visible:        visible,
	}, l.startTimeout())
	if err != nil {
		instance.stop(ctx)
		return nil, err
	}
	instance.process = process

	wsURL, err := launch.DebuggerURL(process.Port, l.startTimeout())
	if err != nil {
		instance.stop(ctx)
		return nil, err
	}
	conn, err := cdp.Dial(wsURL, l.startTimeout())
	if err != nil {
		instance.stop(ctx)
		return nil, fmt.Errorf("dialling the browser: %w", err)
	}
	instance.conn = conn

	driver, err := chrome.Connect(ctx, conn, policy)
	if err != nil {
		// The browser is up and the fence is not. It does not get to stay up: this is the one
		// failure spec §6.2a names, and leaving the process behind would leave an unfenced browser
		// holding the profile.
		instance.stop(ctx)
		return nil, err
	}
	if visible {
		// The visible browser's person turn lifts the proxy too, in-process; see Driver.MakeVisible.
		driver.MakeVisible(proxy.SetPerson)
	}
	instance.Driver = driver
	return instance, nil
}

// LaunchHuman starts a headful browser over a profile directory, with no fence at all.
//
// The missing proxy is spec §6.4 and not an oversight: the restrictions exist because an agent is
// acting, and the whole point of a handover is that a person is. Fencing this window would block the
// POST that submits the login the handover was requested for.
//
// The asymmetry with Launch is worth stating rather than inferring — one of these functions builds a
// browser that may be trusted with the owner's cookies because nothing may act in it, and the other
// builds one that may be trusted because only the owner can.
func (l ChromeLauncher) LaunchHuman(ctx context.Context, dir string) (Instance, error) {
	lifetime, cancel := context.WithCancel(context.Background())
	instance := &humanInstance{cancel: cancel}

	process, err := launch.Start(lifetime, launch.Options{
		ExecutablePath: l.ExecutablePath,
		ProfileDir:     dir,
		Mode:           browser.ModeHuman,
		CacheMB:        l.CacheMB,
	}, l.startTimeout())
	if err != nil {
		instance.stop(ctx)
		return nil, err
	}
	instance.process = process

	wsURL, err := launch.DebuggerURL(process.Port, l.startTimeout())
	if err != nil {
		instance.stop(ctx)
		return nil, err
	}
	conn, err := cdp.Dial(wsURL, l.startTimeout())
	if err != nil {
		instance.stop(ctx)
		return nil, fmt.Errorf("dialling the browser: %w", err)
	}
	instance.conn = conn

	human, err := chrome.ConnectHuman(ctx, conn)
	if err != nil {
		// A headful browser whose recorder never attached is a login whose chain cannot be granted
		// (spec §5.3a). Refused rather than opened: the person would do the work and be offered
		// nothing to keep, which is worse than being told to try again.
		instance.stop(ctx)
		return nil, err
	}
	instance.Human = human
	return instance, nil
}

func (l ChromeLauncher) startTimeout() time.Duration {
	if l.StartTimeout <= 0 {
		return 45 * time.Second
	}
	return l.StartTimeout
}

// chromeInstance is one running browser and everything that has to be taken down with it.
type chromeInstance struct {
	*chrome.Driver
	conn    *cdp.Conn
	process *launch.Process
	proxy   *fence.Proxy
	cancel  context.CancelFunc
}

// Shutdown closes the browser gracefully and then makes sure it is gone.
func (i *chromeInstance) Shutdown(ctx context.Context) { i.stop(ctx) }

// stop is also the failure path of Launch, which is why every field is checked: it runs on a
// half-built instance more often than on a finished one.
//
// # Why it closes before it kills
//
// Spec §4.2 measured this: with Browser.close everything in the profile survives the cycle, and with
// a kill a write from seconds earlier comes back empty — Chrome only flushes the profile on a
// graceful close. The profile is the identity, so the difference between the two is whether a login
// the person made minutes ago is still there next time.
//
// And then it kills anyway. The PID the launcher holds is not the browser (spec §9.3): a close that
// hangs, or a renderer that ignores it, leaves a Chrome running with our argv holding the profile —
// and the next launch over that directory inherits it instead of starting clean.
func (i *chromeInstance) stop(ctx context.Context) {
	if i.conn != nil {
		closing, cancel := context.WithTimeout(withoutCancel(ctx), 5*time.Second)
		_, _ = i.conn.Call(closing, cdp.BrowserSession, "Browser.close", nil)
		select {
		case <-i.conn.Done():
		case <-closing.Done():
		}
		cancel()
		_ = i.conn.Close()
	}
	if i.process != nil {
		i.process.Stop()
	}
	if i.proxy != nil {
		// Last. Until the browser is gone it may still be finishing requests, and a proxy that
		// disappeared underneath them would turn a graceful close into a page full of errors.
		_ = i.proxy.Close()
	}
	if i.cancel != nil {
		i.cancel()
	}
}

// humanInstance is the person's browser: the same process management, none of the fence.
//
// Shutdown goes through the same graceful close for the reason spec §4.2 measured, and here it is the
// one that pays: this is the process the login was made in, and a kill loses the cookie that login
// produced.
type humanInstance struct {
	*chrome.Human
	conn    *cdp.Conn
	process *launch.Process
	cancel  context.CancelFunc
}

func (i *humanInstance) Shutdown(ctx context.Context) { i.stop(ctx) }

func (i *humanInstance) stop(ctx context.Context) {
	if i.Human != nil {
		// Before the connection goes, so nothing Chrome does on the way out is recorded as somewhere
		// the person chose to go.
		i.Human.Detach()
	}
	if i.conn != nil {
		closing, cancel := context.WithTimeout(withoutCancel(ctx), 5*time.Second)
		_, _ = i.conn.Call(closing, cdp.BrowserSession, "Browser.close", nil)
		select {
		case <-i.conn.Done():
		case <-closing.Done():
		}
		cancel()
		_ = i.conn.Close()
	}
	if i.process != nil {
		i.process.Stop()
	}
	if i.cancel != nil {
		i.cancel()
	}
}

// withoutCancel keeps the graceful close from inheriting a cancellation that has already happened.
//
// Shutdown is often called on a path where the caller's context is already dead — a failed open, a
// sidecar shutting down. Inheriting it would skip Browser.close and go straight to the kill, which is
// the case where losing the profile's last write costs a login.
func withoutCancel(ctx context.Context) context.Context {
	if ctx == nil {
		return context.Background()
	}
	return context.WithoutCancel(ctx)
}

package main

import (
	"context"
	"fmt"
	"io"
	"log"
	"sync"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/launch"
	"nucleosbrowser/profile"
)

// Spec §9.5, which until now was a paragraph with no caller.
//
// `launch.Install.Download` has existed since step C, fetches, verifies against the pinned digest and
// extracts — and nothing in production ever called it, because nothing constructed a `Pin` and there
// was no HTTP `Fetcher`. So the pillar's first run needed a person to put a browser on disk by hand,
// which is not what §9.5 says and not what anybody would have discovered until they tried it.
//
// The four things §9.5 asks for, and where each one is:
//
//   - **On activation, never at install or build time.** This runs when the sidecar starts, and the
//     daemon only starts the sidecar when the pillar is enabled.
//   - **Does not block the daemon.** The sidecar serves immediately; what it serves until the archive
//     lands is a refusal that names the reason.
//   - **Has progress.** `countingBody` below, every 25MB, because 350MB of silence is
//     indistinguishable from a hang.
//   - **Retries with backoff.** `retryDelay`, capped, forever. A laptop that opened the lid inside a
//     tunnel should end up with a browser without anybody restarting anything.

// deferrable is everything `serve` type-asserts for. Held as one interface so the swap below moves
// all three surfaces at once — a wrapper that forwarded Driver but kept answering the old Wheelhouse
// would be a browser you could open a session in and never hand over.
type deferrable interface {
	browser.Driver
	browser.Wheelhouse
	browser.Profiles
}

// deferredDriver answers with a named refusal until the pinned Chromium arrives, then answers as the
// pool. One value that is handed to `serve` once and changes underneath it.
//
// The alternative — restarting the process when the download finishes — was rejected: the daemon
// supervises this process and a self-restart during a supervised backoff is two restart policies
// fighting over one process (spec §9.1).
type deferredDriver struct {
	mu     sync.RWMutex
	inner  deferrable
	stopFn func()
}

func newDeferredDriver(reason error) *deferredDriver {
	return &deferredDriver{
		inner:  browser.Unavailable{Reason: reason},
		stopFn: func() {},
	}
}

// swap installs the real driver. The old one is not shut down here because the only thing it ever
// was is an `Unavailable`, which owns nothing.
func (d *deferredDriver) swap(next deferrable, stop func()) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.inner = next
	d.stopFn = stop
}

func (d *deferredDriver) get() deferrable {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return d.inner
}

func (d *deferredDriver) stop() {
	d.mu.RLock()
	stop := d.stopFn
	d.mu.RUnlock()
	stop()
}

func (d *deferredDriver) Name() string { return d.get().Name() }

func (d *deferredDriver) Open(ctx context.Context, req browser.OpenRequest) (browser.Session, error) {
	return d.get().Open(ctx, req)
}

func (d *deferredDriver) Snapshot(ctx context.Context, id browser.SessionID, changesOnly bool) (browser.Snapshot, error) {
	return d.get().Snapshot(ctx, id, changesOnly)
}

func (d *deferredDriver) Act(ctx context.Context, id browser.SessionID, action browser.Action) (browser.ActResult, error) {
	return d.get().Act(ctx, id, action)
}

func (d *deferredDriver) Screenshot(ctx context.Context, id browser.SessionID) ([]byte, error) {
	return d.get().Screenshot(ctx, id)
}

func (d *deferredDriver) Handoff(ctx context.Context, id browser.SessionID, reason string) (browser.HandoffTicket, error) {
	return d.get().Handoff(ctx, id, reason)
}

func (d *deferredDriver) Close(ctx context.Context, id browser.SessionID) error {
	return d.get().Close(ctx, id)
}

func (d *deferredDriver) TakeWheel(ctx context.Context, req browser.WheelRequest) (browser.Wheel, error) {
	return d.get().TakeWheel(ctx, req)
}

func (d *deferredDriver) ReturnWheel(ctx context.Context, id browser.SessionID) (browser.Returned, error) {
	return d.get().ReturnWheel(ctx, id)
}

func (d *deferredDriver) Forget(ctx context.Context, ref profile.Ref) ([]browser.SessionID, error) {
	return d.get().Forget(ctx, ref)
}

// notInstalled is what every request is answered with until the archive lands.
func notInstalled(install launch.Install, detail string) error {
	return fmt.Errorf("%w: chromium %s at %s (%s)",
		browser.ErrNotInstalled, install.Pin.Revision, install.ExecutablePath(), detail)
}

// fetchUntilInstalled downloads the pinned Chromium, retrying for as long as the process lives.
//
// Forever, and bounded only by the delay. A ceiling on attempts would mean a machine that was offline
// for an afternoon needs somebody to notice and restart the sidecar — and the thing that would tell
// them is the health readout they are not looking at, because the pillar is off.
func fetchUntilInstalled(ctx context.Context, install launch.Install, deferred *deferredDriver, ready func() (deferrable, func(), error)) {
	if install.Pin.URL == "" {
		// No pin for this platform. Not worth a retry loop: no amount of waiting produces a URL.
		log.Printf("no pinned chromium for this platform: %v", launch.ErrNoPinForPlatform)
		return
	}

	for attempt := 1; ; attempt++ {
		log.Printf("downloading chromium %s (attempt %d) from %s",
			install.Pin.Revision, attempt, install.Pin.URL)
		started := time.Now()
		err := install.Download(ctx, &progressFetcher{})
		if err == nil {
			log.Printf("chromium %s installed in %s", install.Pin.Revision, time.Since(started).Round(time.Second))
			driver, stop, err := ready()
			if err != nil {
				// The archive is on disk and the pool would not build over it. Retrying the DOWNLOAD
				// would not help — the bytes are already verified — so this stops and says so.
				log.Printf("chromium is installed and the pool would not start: %v", err)
				deferred.swap(browser.Unavailable{Reason: err}, func() {})
				return
			}
			deferred.swap(driver, stop)
			return
		}

		if ctx.Err() != nil {
			return
		}
		delay := retryDelay(attempt)
		log.Printf("downloading chromium failed (%v); retrying in %s", err, delay)
		deferred.swap(browser.Unavailable{
			Reason: notInstalled(install, fmt.Sprintf("attempt %d failed: %v", attempt, err)),
		}, func() {})

		select {
		case <-ctx.Done():
			return
		case <-time.After(delay):
		}
	}
}

// retryDelay is 5s, 15s, 45s … capped at ten minutes.
//
// Tripling rather than doubling because the first failure is almost always "no network yet" and the
// tenth is almost always "this network will not serve it"; a doubling schedule spends its first
// twenty attempts inside the first case.
func retryDelay(attempt int) time.Duration {
	const (
		base = 5 * time.Second
		max  = 10 * time.Minute
	)
	delay := base
	for i := 1; i < attempt && delay < max; i++ {
		delay *= 3
	}
	if delay > max {
		return max
	}
	return delay
}

// progressFetcher is the production fetcher with a line of log every 25MB.
//
// It wraps rather than extends `launch.HTTPFetcher`, so nothing about progress reporting can affect
// what bytes reach the digest check.
type progressFetcher struct{ inner launch.HTTPFetcher }

func (f *progressFetcher) Fetch(ctx context.Context, url string) (io.ReadCloser, error) {
	body, err := f.inner.Fetch(ctx, url)
	if err != nil {
		return nil, err
	}
	return &countingBody{inner: body}, nil
}

// countingBody logs how much has arrived. 350MB of silence is indistinguishable from a hang, and the
// person watching this log has no other way to tell the two apart.
type countingBody struct {
	inner    io.ReadCloser
	read     int64
	reported int64
}

const reportEvery = 25 << 20

func (c *countingBody) Read(p []byte) (int, error) {
	n, err := c.inner.Read(p)
	c.read += int64(n)
	if c.read-c.reported >= reportEvery {
		c.reported = c.read
		log.Printf("chromium: %d MB downloaded", c.read>>20)
	}
	return n, err
}

func (c *countingBody) Close() error { return c.inner.Close() }

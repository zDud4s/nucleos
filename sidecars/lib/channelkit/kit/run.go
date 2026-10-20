package kit

import (
	"context"
	"errors"
	"io"
	"os"
	"sync"
	"time"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/lifeline"
)

// runShutdownGrace is how long Run waits for in-flight work once a stop is asked
// for, same as the v1 Telegram sidecar (shutdownGrace).
const runShutdownGrace = 15 * time.Second

// Process seams for tests: where the lifeline is read from and how the
// environment is looked up.
var (
	lifelineIn io.Reader = os.Stdin
	runGetenv            = os.Getenv
)

// Ingest is the inbound side of an adapter: it reads the platform and posts events
// to the Sink until ctx ends (returning nil) or something unrecoverable happens.
type Ingest func(ctx context.Context) error

// Factory builds the Adapter and its Ingest. It gets the Client (the adapter needs it
// for UploadMedia and DownloadMedia) and the Sink for inbound events.
type Factory func(c *Client, sink Sink) (Adapter, Ingest, error)

// Run is the whole life of a channel adapter process: lifeline, then the Runner
// (hello, reconnection, outbox) and Ingest side by side. A fatal error on either
// side cancels the other and is returned; so is a refused hello (ErrIncompatible).
// A cancelled ctx or a cut lifeline is an orderly stop and returns nil, after
// in-flight work settled or the shutdown grace elapsed.
func Run(ctx context.Context, cfg Config, f Factory) error {
	switch {
	case cfg.BaseURL == "":
		return errors.New("channelkit: daemon URL is required")
	case cfg.Token == "":
		return errors.New("channelkit: channel token is required")
	case cfg.Channel == "":
		return errors.New("channelkit: channel id is required")
	case f == nil:
		return errors.New("channelkit: factory is required")
	}
	client := NewClient(cfg)
	var clk clock.Clock = client.clock
	stats := &Stats{}

	adapter, ingest, err := f(client.WithStats(stats), *NewSink(client, stats))
	if err != nil {
		return err
	}
	if adapter == nil || ingest == nil {
		return errors.New("channelkit: factory returned a nil adapter or ingest")
	}

	rctx, cancel := context.WithCancel(ctx)
	defer cancel()
	lifeline.Watch(runGetenv, lifelineIn, cancel)

	runner := &Runner{
		Client: client, Adapter: adapter, Clock: clk, Stats: stats,
		Guard: Guard, ShutdownGrace: runShutdownGrace,
	}

	var (
		mu       sync.Mutex
		firstErr error
		wg       sync.WaitGroup
	)
	finish := func(err error) {
		mu.Lock()
		if err != nil && firstErr == nil {
			firstErr = err
		}
		mu.Unlock()
		cancel()
	}
	wg.Add(2)
	go func() { defer wg.Done(); finish(runner.Run(rctx)) }()
	go func() { defer wg.Done(); finish(ingest(rctx)) }()

	done := make(chan struct{})
	go func() { wg.Wait(); close(done) }()
	select {
	case <-done:
	case <-rctx.Done():
		// Runner.Run enforces the grace on its own side; ingest gets the same.
		timer := clk.NewTimer(runShutdownGrace)
		defer timer.Stop()
		select {
		case <-done:
		case <-timer.C():
			logf("channelkit: ingest did not stop within %s, abandoning it", runShutdownGrace)
		}
	}
	mu.Lock()
	defer mu.Unlock()
	return firstErr
}

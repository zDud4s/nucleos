package kit

import (
	"context"
	"errors"
	"io"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

func envOf(m map[string]string) func(string) string {
	return func(k string) string { return m[k] }
}

func TestConfigFromEnvReadsURLTokenAndChannel(t *testing.T) {
	cfg, err := ConfigFromEnv(envOf(map[string]string{
		"NUCLEOS_DAEMON_URL":    "http://127.0.0.1:8791/",
		"NUCLEOS_CHANNEL_TOKEN": "tok",
		"NUCLEOS_CHANNEL_ID":    "tg-main",
	}))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.BaseURL != "http://127.0.0.1:8791/" || cfg.Token != "tok" || cfg.Channel != "tg-main" {
		t.Fatalf("cfg = %+v", cfg)
	}
}

func TestConfigFromEnvChannelIDIsOptional(t *testing.T) {
	cfg, err := ConfigFromEnv(envOf(map[string]string{
		"NUCLEOS_DAEMON_URL":    "http://localhost:1",
		"NUCLEOS_CHANNEL_TOKEN": "tok",
	}))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Channel != "" {
		t.Fatalf("channel = %q, want empty (the adapter may set it)", cfg.Channel)
	}
}

func TestConfigFromEnvRequiresToken(t *testing.T) {
	_, err := ConfigFromEnv(envOf(map[string]string{"NUCLEOS_DAEMON_URL": "http://127.0.0.1:1"}))
	if err == nil || !strings.Contains(err.Error(), "NUCLEOS_CHANNEL_TOKEN") {
		t.Fatalf("err = %v, want one naming NUCLEOS_CHANNEL_TOKEN", err)
	}
}

func TestConfigFromEnvRequiresDaemonURL(t *testing.T) {
	_, err := ConfigFromEnv(envOf(map[string]string{"NUCLEOS_CHANNEL_TOKEN": "tok"}))
	if err == nil || !strings.Contains(err.Error(), "NUCLEOS_DAEMON_URL") {
		t.Fatalf("err = %v, want one naming NUCLEOS_DAEMON_URL", err)
	}
}

func TestConfigFromEnvRefusesANonLoopbackURL(t *testing.T) {
	for _, raw := range []string{
		"http://example.com:8791", "https://example.com", "http://10.0.0.5:1",
		"ftp://127.0.0.1", "127.0.0.1:8791", "http://", "://bad",
	} {
		_, err := ConfigFromEnv(envOf(map[string]string{"NUCLEOS_DAEMON_URL": raw, "NUCLEOS_CHANNEL_TOKEN": "tok-secret"}))
		if err == nil {
			t.Errorf("url %q accepted, want an error", raw)
			continue
		}
		if strings.Contains(err.Error(), "tok-secret") {
			t.Errorf("error leaks the token: %v", err)
		}
	}
}

func TestConfigFromEnvAcceptsLoopbackHosts(t *testing.T) {
	for _, raw := range []string{"http://127.0.0.1:8791", "http://localhost:8791", "http://[::1]:8791", "https://127.0.0.1:1"} {
		if _, err := ConfigFromEnv(envOf(map[string]string{"NUCLEOS_DAEMON_URL": raw, "NUCLEOS_CHANNEL_TOKEN": "tok"})); err != nil {
			t.Errorf("url %q: %v", raw, err)
		}
	}
}

// runRig is Run against the fake core.
type runRig struct {
	t    *testing.T
	core *fakecore.Core
	clk  *clock.Fake
	cfg  Config
	ad   *loopAdapter
}

func newRunRig(t *testing.T) *runRig {
	t.Helper()
	clk := clock.NewFake(time.Date(2026, 10, 19, 10, 0, 0, 0, time.UTC))
	core := fakecore.New(t, clk)
	return &runRig{t: t, core: core, clk: clk, ad: &loopAdapter{}, cfg: Config{
		BaseURL: core.URL(), Channel: core.Channel(), Token: core.Token(), Clock: clk,
	}}
}

// factory hands back the rig's adapter and ingest (nil: wait for ctx).
func (g *runRig) factory(got func(*Client, Sink), ingest Ingest) Factory {
	return func(c *Client, s Sink) (Adapter, Ingest, error) {
		if got != nil {
			got(c, s)
		}
		if ingest == nil {
			ingest = func(ctx context.Context) error { <-ctx.Done(); return nil }
		}
		return g.ad, ingest, nil
	}
}

func (g *runRig) start(ctx context.Context, f Factory) <-chan error {
	done := make(chan error, 1)
	go func() { done <- Run(ctx, g.cfg, f) }()
	return done
}

func waitRun(t *testing.T, done <-chan error) error {
	t.Helper()
	select {
	case err := <-done:
		return err
	case <-time.After(10 * time.Second):
		t.Fatal("Run did not return")
		return nil
	}
}

func (g *runRig) waitDelivery(id uint64, want fakecore.State) {
	g.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for {
		if st, _, ok := g.core.Delivery(id); ok && st == want {
			return
		}
		if time.Now().After(deadline) {
			st, _, _ := g.core.Delivery(id)
			g.t.Fatalf("action %d: state %q, want %q", id, st, want)
		}
		time.Sleep(time.Millisecond)
	}
}

func TestRunStartsHelloIngestAndDeliversActions(t *testing.T) {
	g := newRunRig(t)
	var mu sync.Mutex
	var sink Sink
	posted := make(chan struct{})
	ingest := func(ctx context.Context) error {
		mu.Lock()
		s := sink
		mu.Unlock()
		if _, err := s.Post(ctx, sinkEvent(t, "ev-1")); err != nil {
			return err
		}
		close(posted)
		<-ctx.Done()
		return nil
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := g.start(ctx, g.factory(func(_ *Client, s Sink) { mu.Lock(); sink = s; mu.Unlock() }, ingest))

	id := g.core.Enqueue(loopSend("c1", "hi"))
	g.waitDelivery(id, fakecore.StateDone)
	select {
	case <-posted:
	case <-time.After(10 * time.Second):
		t.Fatal("event was never posted")
	}
	if evs := g.core.Events(); len(evs) != 1 {
		t.Fatalf("events = %d, want 1", len(evs))
	}
	if h, n := g.core.Hello(); n < 1 || h.Version != contract.Version {
		t.Fatalf("hello = %+v (%d), want one at version %s", h, n, contract.Version)
	}
	cancel()
	if err := waitRun(t, done); err != nil {
		t.Fatalf("Run = %v after cancel, want nil", err)
	}
}

func TestRunHelloConflictReturnsErrIncompatible(t *testing.T) {
	g := newRunRig(t)
	g.core.FailNext(fakecore.RouteHello, 1, 409, `{"error":"contract_version","contract_version":"9.0.0"}`)
	done := g.start(context.Background(), g.factory(nil, nil))
	err := waitRun(t, done)
	if !errors.Is(err, ErrIncompatible) {
		t.Fatalf("Run = %v, want ErrIncompatible", err)
	}
	if n := g.core.Calls(fakecore.RouteOutbox); n != 0 {
		t.Fatalf("outbox polled %d times after a refused hello, want 0", n)
	}
}

func TestRunRejectsAnIncompleteConfig(t *testing.T) {
	g := newRunRig(t)
	for name, mut := range map[string]func(*Config){
		"no token":   func(c *Config) { c.Token = "" },
		"no url":     func(c *Config) { c.BaseURL = "" },
		"no channel": func(c *Config) { c.Channel = "" },
	} {
		cfg := g.cfg
		mut(&cfg)
		if err := Run(context.Background(), cfg, g.factory(nil, nil)); err == nil {
			t.Errorf("%s: Run = nil, want an error", name)
		}
	}
}

func TestRunFactoryErrorIsReturned(t *testing.T) {
	g := newRunRig(t)
	boom := errors.New("boom")
	err := Run(context.Background(), g.cfg, func(*Client, Sink) (Adapter, Ingest, error) { return nil, nil, boom })
	if !errors.Is(err, boom) {
		t.Fatalf("Run = %v, want boom", err)
	}
}

func TestRunFactoryGetsAUsableClient(t *testing.T) {
	g := newRunRig(t)
	uploaded := make(chan string, 1)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := g.start(ctx, g.factory(func(c *Client, _ Sink) {
		go func() {
			id, err := c.UploadMedia(ctx, "image/png", strings.NewReader("png-bytes"))
			if err != nil {
				id = "err:" + err.Error()
			}
			uploaded <- id
		}()
	}, nil))
	var id string
	select {
	case id = <-uploaded:
	case <-time.After(10 * time.Second):
		t.Fatal("upload did not finish")
	}
	data, mime, ok := g.core.Media(id)
	if !ok || string(data) != "png-bytes" || mime != "image/png" {
		t.Fatalf("media %q = %q %q %v", id, data, mime, ok)
	}
	cancel()
	waitRun(t, done)
}

func TestRunWiresTheCapabilityGuard(t *testing.T) {
	g := newRunRig(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := g.start(ctx, g.factory(nil, nil))
	// loopAdapter declares no buttons: a send with buttons must be vetoed by Guard.
	a := contract.Send{
		Conversation: "c1",
		Parts:        []contract.OutPart{{Type: contract.OutText, Text: "x"}},
		Buttons:      [][]contract.Button{{{Label: "Yes", Token: "tk_1"}}},
	}
	id := g.core.Enqueue(a)
	g.waitDelivery(id, fakecore.StateFailed)
	if n := len(g.ad.executions()); n != 0 {
		t.Fatalf("Execute ran %d times, want 0 (Guard must veto)", n)
	}
	cancel()
	waitRun(t, done)
}

func TestRunLifelineCutCancelsAndReturnsNil(t *testing.T) {
	g := newRunRig(t)
	pr, pw := io.Pipe()
	t.Setenv("NUCLEOS_LIFELINE", "1")
	oldIn := lifelineIn
	lifelineIn = pr
	t.Cleanup(func() { lifelineIn = oldIn })

	started := make(chan struct{})
	ingest := func(ctx context.Context) error { close(started); <-ctx.Done(); return nil }
	done := g.start(context.Background(), g.factory(nil, ingest))
	<-started
	select {
	case err := <-done:
		t.Fatalf("Run returned early: %v", err)
	case <-time.After(50 * time.Millisecond):
	}
	pw.Close()
	if err := waitRun(t, done); err != nil {
		t.Fatalf("Run = %v after a cut lifeline, want nil", err)
	}
}

func TestRunWaitsForInflightExecutionsOnCancel(t *testing.T) {
	g := newRunRig(t)
	entered := make(chan struct{})
	release := make(chan struct{})
	g.ad.exec = func(context.Context, contract.Action) ([]string, error) {
		close(entered)
		<-release
		return []string{"m1"}, nil
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := g.start(ctx, g.factory(nil, nil))
	id := g.core.Enqueue(loopSend("c1", "hi"))
	select {
	case <-entered:
	case <-time.After(10 * time.Second):
		t.Fatal("Execute never started")
	}
	cancel()
	select {
	case err := <-done:
		t.Fatalf("Run returned (%v) with an execution in flight", err)
	case <-time.After(100 * time.Millisecond):
	}
	close(release)
	if err := waitRun(t, done); err != nil {
		t.Fatalf("Run = %v, want nil", err)
	}
	g.waitDelivery(id, fakecore.StateDone)
}

func TestRunIngestErrorStopsTheRunnerAndIsReturned(t *testing.T) {
	g := newRunRig(t)
	boom := errors.New("ingest broke")
	done := g.start(context.Background(), g.factory(nil, func(context.Context) error { return boom }))
	if err := waitRun(t, done); !errors.Is(err, boom) {
		t.Fatalf("Run = %v, want the ingest error", err)
	}
}

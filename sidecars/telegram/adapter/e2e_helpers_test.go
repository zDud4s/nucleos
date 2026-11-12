package adapter

import (
	"context"
	"errors"
	"io"
	"runtime"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
	"nucleoschannelkit/kit"

	"nucleostelegram/config"
	"nucleostelegram/telegram"
)

// e2eWait bounds every wait of the e2e tests in real time; nothing here sleeps, a wait either
// observes its condition or runs into this deadline and fails.
const e2eWait = 10 * time.Second

// eventually spins (yielding the processor, never sleeping) until cond holds.
func eventually(t *testing.T, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(e2eWait)
	for !cond() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		runtime.Gosched()
	}
}

// e2eBot is the fake Bot API of the e2e tests: SendBot, PollBot and FileBot in one. getUpdates behaves
// like the real one: it answers every pushed update at or past the requested offset, so an update that
// was not confirmed by a higher offset comes back.
type e2eBot struct {
	mu      sync.Mutex
	updates []telegram.Update
	wake    chan struct{}
	offsets []int64
	sends   []telegram.SendParams
	actions []actionCall
	answers []answerCall

	// sendFn, when set, decides the outcome of the n-th (1-based) text send. It runs without the lock.
	sendFn func(n int, p telegram.SendParams) (int64, error)
	// signal receives one token per observable call (send, chat action, callback answer).
	signal chan struct{}
}

func newE2EBot() *e2eBot {
	return &e2eBot{wake: make(chan struct{}), signal: make(chan struct{}, 64)}
}

// push makes updates available to getUpdates.
func (b *e2eBot) push(us ...telegram.Update) {
	b.mu.Lock()
	b.updates = append(b.updates, us...)
	close(b.wake)
	b.wake = make(chan struct{})
	b.mu.Unlock()
}

func (b *e2eBot) GetUpdatesContext(ctx context.Context, offset int64, _ int) ([]telegram.Update, error) {
	b.mu.Lock()
	b.offsets = append(b.offsets, offset)
	b.mu.Unlock()
	for {
		b.mu.Lock()
		var out []telegram.Update
		for _, u := range b.updates {
			if u.UpdateID >= offset {
				out = append(out, u)
			}
		}
		wake := b.wake
		b.mu.Unlock()
		if len(out) > 0 {
			return out, nil
		}
		select {
		case <-wake:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
}

func (b *e2eBot) note() { b.signal <- struct{}{} }

func (b *e2eBot) SendHTMLEx(_ context.Context, p telegram.SendParams) (int64, error) {
	b.mu.Lock()
	b.sends = append(b.sends, p)
	n := len(b.sends)
	fn := b.sendFn
	b.mu.Unlock()
	defer b.note()
	if fn != nil {
		return fn(n, p)
	}
	return int64(5000 + n), nil
}

func (b *e2eBot) EditMessageText(context.Context, int64, int64, string, string, [][]telegram.Button) error {
	return nil
}

func (b *e2eBot) SendChatAction(_ context.Context, to telegram.Destination, action string) error {
	b.mu.Lock()
	b.actions = append(b.actions, actionCall{to, action})
	b.mu.Unlock()
	b.note()
	return nil
}

func (b *e2eBot) AnswerCallbackQueryCtx(_ context.Context, id, text string) error {
	b.mu.Lock()
	b.answers = append(b.answers, answerCall{id, text})
	b.mu.Unlock()
	b.note()
	return nil
}

func (b *e2eBot) SendMediaEx(context.Context, telegram.MediaKind, telegram.SendMediaParams, io.Reader) (telegram.MediaSent, error) {
	return telegram.MediaSent{}, errors.New("e2eBot: media is not used by these tests")
}

func (b *e2eBot) GetFile(string) (string, error)      { return "", errors.New("e2eBot: no files") }
func (b *e2eBot) DownloadFile(string) ([]byte, error) { return nil, errors.New("e2eBot: no files") }

func (b *e2eBot) sendCount() int {
	b.mu.Lock()
	defer b.mu.Unlock()
	return len(b.sends)
}

func (b *e2eBot) sent() []telegram.SendParams {
	b.mu.Lock()
	defer b.mu.Unlock()
	return append([]telegram.SendParams(nil), b.sends...)
}

// waitSignal waits for n observable calls.
func (b *e2eBot) waitSignal(t *testing.T, n int) {
	t.Helper()
	for i := 0; i < n; i++ {
		select {
		case <-b.signal:
		case <-time.After(e2eWait):
			t.Fatalf("timed out waiting for Bot API call %d of %d", i+1, n)
		}
	}
}

// e2eUploader is the main.go mediaUploader: kit.Client.UploadMedia behind the adapter's Uploader.
type e2eUploader struct{ c *kit.Client }

func (u e2eUploader) Upload(ctx context.Context, mime string, r io.Reader) (string, error) {
	return u.c.UploadMedia(ctx, mime, r)
}

// e2eRig wires the kit, the fake core and a fake Bot API the way main.go does, on one
// shared clock.Fake.
type e2eRig struct {
	t      *testing.T
	clk    *clock.Fake
	core   *fakecore.Core
	bot    *e2eBot
	cfg    kit.Config
	offset string

	mu     sync.Mutex
	cancel context.CancelFunc
	done   chan error
}

func newE2ERig(t *testing.T) *e2eRig {
	t.Helper()
	clk := clock.NewFake(time.Date(2026, 10, 19, 10, 0, 0, 0, time.UTC))
	core := fakecore.New(t, clk)
	g := &e2eRig{
		t: t, clk: clk, core: core, bot: newE2EBot(),
		offset: t.TempDir() + "/offset",
		cfg: kit.Config{
			BaseURL: core.URL(), Channel: core.Channel(), Token: core.Token(), Kind: "telegram", Clock: clk,
		},
	}
	t.Cleanup(func() { g.stop() })
	return g
}

// start launches kit.Run with the same factory as main.go. A rig can start a second "process" after
// the first one stopped (crash scenarios), with a fresh bot if bot is non-nil.
func (g *e2eRig) start(bot *e2eBot) {
	g.t.Helper()
	if bot != nil {
		g.bot = bot
	}
	b := g.bot
	factory := func(c *kit.Client, sink kit.Sink) (kit.Adapter, kit.Ingest, error) {
		attacher := MediaAttacher{Bot: b, Up: e2eUploader{c: c}}
		ingest := func(ctx context.Context) error {
			return Poll(ctx, b, attacher, &sink, g.offset, g.clk)
		}
		return New(b, c), ingest, nil
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	g.mu.Lock()
	g.cancel, g.done = cancel, done
	g.mu.Unlock()
	go func() { done <- kit.Run(ctx, g.cfg, factory) }()
}

// stop ends the running kit.Run. It is safe to call again afterwards.
func (g *e2eRig) stop() {
	g.mu.Lock()
	cancel, done := g.cancel, g.done
	g.cancel, g.done = nil, nil
	g.mu.Unlock()
	if cancel == nil {
		return
	}
	cancel()
	select {
	case <-done:
	case <-time.After(e2eWait):
		g.t.Error("kit.Run did not return after stop")
	}
}

// kill cancels the running kit.Run without waiting for it (usable from inside a Bot API call).
func (g *e2eRig) kill() {
	g.mu.Lock()
	cancel := g.cancel
	g.mu.Unlock()
	if cancel != nil {
		cancel()
	}
}

// waitKilled returns once the Run cancelled by kill has returned.
func (g *e2eRig) waitKilled() {
	g.t.Helper()
	g.mu.Lock()
	done := g.done
	g.cancel, g.done = nil, nil
	g.mu.Unlock()
	select {
	case err := <-done:
		if err != nil {
			g.t.Errorf("kit.Run after kill = %v, want nil", err)
		}
	case <-time.After(e2eWait):
		g.t.Fatal("kit.Run did not return after kill")
	}
}

// pending waits for exactly n armed timers on the shared clock: the parked outbox long-poll accounts
// for one, a lease renewal or a backoff sleep for the others.
func (g *e2eRig) pending(n int) {
	g.t.Helper()
	eventually(g.t, "clock timers", func() bool { return g.clk.Pending() == n })
}

func (g *e2eRig) waitState(id uint64, want fakecore.State) {
	g.t.Helper()
	eventually(g.t, "outbox state "+string(want), func() bool {
		st, _, ok := g.core.Delivery(id)
		return ok && st == want
	})
}

func (g *e2eRig) waitCalls(route string, n int) {
	g.t.Helper()
	eventually(g.t, "calls to "+route, func() bool { return g.core.Calls(route) >= n })
}

func (g *e2eRig) events(n int) []contract.Event {
	g.t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), e2eWait)
	defer cancel()
	evs, err := g.core.WaitEvents(ctx, n)
	if err != nil {
		g.t.Fatalf("waiting for %d events: %v", n, err)
	}
	return evs
}

func (g *e2eRig) savedOffset() int64 { return config.LoadOffset(g.offset) }

func e2eText(conv, text string) contract.Send {
	return contract.Send{Conversation: conv, Parts: []contract.OutPart{{Type: contract.OutText, Text: text}}}
}

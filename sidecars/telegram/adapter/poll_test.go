package adapter

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
	"nucleoschannelkit/kit"

	"nucleostelegram/config"
	"nucleostelegram/telegram"
)

// pollStep is one scripted getUpdates answer.
type pollStep struct {
	updates []telegram.Update
	err     error
}

// scriptedBot answers getUpdates from a script. Once the script is spent it calls onDone and then
// blocks until ctx ends, like a long poll with nothing new.
type scriptedBot struct {
	mu      sync.Mutex
	steps   []pollStep
	offsets []int64
	called  chan struct{}
	onDone  func()
}

func (b *scriptedBot) GetUpdatesContext(ctx context.Context, offset int64, _ int) ([]telegram.Update, error) {
	b.mu.Lock()
	b.offsets = append(b.offsets, offset)
	var step pollStep
	spent := len(b.steps) == 0
	if !spent {
		step, b.steps = b.steps[0], b.steps[1:]
	}
	b.mu.Unlock()
	if b.called != nil {
		select {
		case b.called <- struct{}{}:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	if spent {
		if b.onDone != nil {
			b.onDone()
		}
		<-ctx.Done()
		return nil, ctx.Err()
	}
	return step.updates, step.err
}

func (b *scriptedBot) seenOffsets() []int64 {
	b.mu.Lock()
	defer b.mu.Unlock()
	return append([]int64(nil), b.offsets...)
}

// fakePoster records what reaches the core and answers through post.
type fakePoster struct {
	mu     sync.Mutex
	events []contract.Event
	post   func(ctx context.Context, ev contract.Event) (kit.Outcome, error)
	log    *[]string
}

func (p *fakePoster) Post(ctx context.Context, ev contract.Event) (kit.Outcome, error) {
	p.mu.Lock()
	p.events = append(p.events, ev)
	if p.log != nil {
		*p.log = append(*p.log, "post:"+*ev.EventID)
	}
	p.mu.Unlock()
	if p.post != nil {
		return p.post(ctx, ev)
	}
	return kit.Outcome{Advance: true}, nil
}

func (p *fakePoster) ids() []string {
	p.mu.Lock()
	defer p.mu.Unlock()
	var out []string
	for _, e := range p.events {
		out = append(out, *e.EventID)
	}
	return out
}

type fakeAttacher struct {
	err error
	log *[]string
}

func (a fakeAttacher) Attach(_ context.Context, ev *contract.Event, _ *telegram.Message) error {
	if a.log != nil {
		*a.log = append(*a.log, "attach:"+*ev.EventID)
	}
	return a.err
}

func offsetFile(t *testing.T) string {
	t.Helper()
	return filepath.Join(t.TempDir(), "offset")
}

// runPoll runs Poll to the end and returns its error.
func runPoll(t *testing.T, ctx context.Context, bot PollBot, att Attacher, sink Poster, path string, clk clock.Clock) error {
	t.Helper()
	done := make(chan error, 1)
	go func() { done <- Poll(ctx, bot, att, sink, path, clk) }()
	select {
	case err := <-done:
		return err
	case <-time.After(10 * time.Second):
		t.Fatal("Poll did not return")
		return nil
	}
}

func TestPollLoopSavesOffsetOnlyAfterPostAdvances(t *testing.T) {
	path := offsetFile(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	bot := &scriptedBot{
		steps:  []pollStep{{updates: []telegram.Update{privateText(10, 1, "a"), privateText(11, 2, "b")}}},
		onDone: cancel,
	}
	var seen []int64
	sink := &fakePoster{post: func(context.Context, contract.Event) (kit.Outcome, error) {
		// At the moment of Post the offset must still be the one before this update.
		seen = append(seen, config.LoadOffset(path))
		return kit.Outcome{Advance: true}, nil
	}}
	if err := runPoll(t, ctx, bot, fakeAttacher{}, sink, path, clock.NewFake(time.Unix(0, 0))); err != nil {
		t.Fatalf("Poll: %v", err)
	}
	if want := []int64{0, 11}; !reflect.DeepEqual(seen, want) {
		t.Fatalf("offset at Post time = %v, want %v", seen, want)
	}
	if got := config.LoadOffset(path); got != 12 {
		t.Fatalf("final offset = %d, want 12", got)
	}
	if want := []int64{0, 12}; !reflect.DeepEqual(bot.seenOffsets(), want) {
		t.Fatalf("getUpdates offsets = %v, want %v", bot.seenOffsets(), want)
	}
}

func TestPollLoopPermanentRejectionStillAdvances(t *testing.T) {
	// A permanent 400 comes back from kit.Sink as Advance (the Sink counts the failure itself), so
	// the offset moves past the update instead of blocking the cursor forever.
	path := offsetFile(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	bot := &scriptedBot{steps: []pollStep{{updates: []telegram.Update{privateText(20, 1, "bad"), privateText(21, 2, "ok")}}}, onDone: cancel}
	var rejected int
	sink := &fakePoster{post: func(_ context.Context, ev contract.Event) (kit.Outcome, error) {
		if *ev.EventID == mustEventID(privateText(20, 1, "bad")) {
			rejected++
		}
		return kit.Outcome{Advance: true}, nil
	}}
	if err := runPoll(t, ctx, bot, fakeAttacher{}, sink, path, clock.NewFake(time.Unix(0, 0))); err != nil {
		t.Fatalf("Poll: %v", err)
	}
	if rejected != 1 || len(sink.ids()) != 2 {
		t.Fatalf("rejected=%d posted=%d", rejected, len(sink.ids()))
	}
	if got := config.LoadOffset(path); got != 22 {
		t.Fatalf("offset = %d, want 22", got)
	}
}

func mustEventID(u telegram.Update) string {
	ev, _, _ := Inbound(u)
	return *ev.EventID
}

func TestPollLoopCancelDuringPostDoesNotSaveAndRedelivers(t *testing.T) {
	path := offsetFile(t)
	if err := config.SaveOffset(path, 30); err != nil {
		t.Fatal(err)
	}
	update := privateText(30, 1, "hello")

	ctx, cancel := context.WithCancel(context.Background())
	bot := &scriptedBot{steps: []pollStep{{updates: []telegram.Update{update}}}}
	first := &fakePoster{post: func(ctx context.Context, _ contract.Event) (kit.Outcome, error) {
		cancel() // shutdown arrives while the core is unreachable
		return kit.Outcome{}, ctx.Err()
	}}
	if err := runPoll(t, ctx, bot, fakeAttacher{}, first, path, clock.NewFake(time.Unix(0, 0))); err != nil {
		t.Fatalf("Poll after cancel: %v", err)
	}
	if got := config.LoadOffset(path); got != 30 {
		t.Fatalf("offset moved to %d on a cancelled Post", got)
	}

	// Restart: the same update is fetched again and carries the same event_id.
	ctx2, cancel2 := context.WithCancel(context.Background())
	defer cancel2()
	bot2 := &scriptedBot{steps: []pollStep{{updates: []telegram.Update{update}}}, onDone: cancel2}
	second := &fakePoster{}
	if err := runPoll(t, ctx2, bot2, fakeAttacher{}, second, path, clock.NewFake(time.Unix(0, 0))); err != nil {
		t.Fatalf("Poll restart: %v", err)
	}
	if bot2.seenOffsets()[0] != 30 {
		t.Fatalf("restart polled from %d, want 30", bot2.seenOffsets()[0])
	}
	if !reflect.DeepEqual(first.ids(), second.ids()) || len(second.ids()) != 1 {
		t.Fatalf("event ids differ across restart: %v vs %v", first.ids(), second.ids())
	}
	if got := config.LoadOffset(path); got != 31 {
		t.Fatalf("offset after redelivery = %d, want 31", got)
	}
}

func TestPollLoopPersistentErrorDoesNotSaveAndStops(t *testing.T) {
	path := offsetFile(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	boom := errors.New("core refused: fatal")
	bot := &scriptedBot{steps: []pollStep{{updates: []telegram.Update{privateText(40, 1, "a"), privateText(41, 2, "b")}}}}
	sink := &fakePoster{post: func(context.Context, contract.Event) (kit.Outcome, error) {
		return kit.Outcome{}, boom
	}}
	err := runPoll(t, ctx, bot, fakeAttacher{}, sink, path, clock.NewFake(time.Unix(0, 0)))
	if !errors.Is(err, boom) {
		t.Fatalf("Poll error = %v, want %v", err, boom)
	}
	if _, statErr := os.Stat(path); !os.IsNotExist(statErr) {
		t.Fatalf("offset file written despite no Advance (stat err %v)", statErr)
	}
	if len(sink.ids()) != 1 {
		t.Fatalf("later updates were posted after a failure: %v", sink.ids())
	}
}

func TestPollLoopStartsFromPersistedOffset(t *testing.T) {
	path := offsetFile(t)
	if err := config.SaveOffset(path, 777); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	bot := &scriptedBot{onDone: cancel}
	if err := runPoll(t, ctx, bot, fakeAttacher{}, &fakePoster{}, path, clock.NewFake(time.Unix(0, 0))); err != nil {
		t.Fatalf("Poll: %v", err)
	}
	if got := bot.seenOffsets(); !reflect.DeepEqual(got, []int64{777}) {
		t.Fatalf("offsets = %v", got)
	}
}

func TestPollLoopGetUpdatesErrorBacksOffTwoToSixtySeconds(t *testing.T) {
	path := offsetFile(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	clk := clock.NewFake(time.Unix(0, 0))
	delays := []time.Duration{2, 4, 8, 16, 32, 60, 60}
	steps := make([]pollStep, len(delays))
	for i := range steps {
		steps[i] = pollStep{err: errors.New("network down")}
	}
	bot := &scriptedBot{steps: steps, called: make(chan struct{})}

	done := make(chan error, 1)
	go func() { done <- Poll(ctx, bot, fakeAttacher{}, &fakePoster{}, path, clk) }()

	recv := func() {
		select {
		case <-bot.called:
		case <-time.After(10 * time.Second):
			t.Fatal("getUpdates was not called again")
		}
	}
	recv() // first failure
	for _, d := range delays {
		clk.BlockUntil(1)
		clk.Advance(d * time.Second)
		recv() // the retry only happens if the timer was no longer than d
	}
	cancel()
	if err := <-done; err != nil {
		t.Fatalf("Poll: %v", err)
	}
}

func TestPollLoopUntranslatableUpdatesAdvanceOffset(t *testing.T) {
	path := offsetFile(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	noMessage := telegram.Update{UpdateID: 50}
	badCallback := telegram.Update{UpdateID: 51, CallbackQuery: &telegram.CallbackQuery{ID: "q", Data: ""}}
	bot := &scriptedBot{steps: []pollStep{{updates: []telegram.Update{noMessage, badCallback}}}, onDone: cancel}
	sink := &fakePoster{}
	if err := runPoll(t, ctx, bot, fakeAttacher{}, sink, path, clock.NewFake(time.Unix(0, 0))); err != nil {
		t.Fatalf("Poll: %v", err)
	}
	if len(sink.ids()) != 0 {
		t.Fatalf("untranslatable updates were posted: %v", sink.ids())
	}
	if got := config.LoadOffset(path); got != 52 {
		t.Fatalf("offset = %d, want 52", got)
	}
}

func TestPollLoopPreservesOrderAndAttachesMediaBeforePost(t *testing.T) {
	path := offsetFile(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	voice := privateText(61, 2, "")
	voice.Message.Voice = &telegram.Voice{FileID: "v1"}
	button := telegram.Update{UpdateID: 62, CallbackQuery: &telegram.CallbackQuery{
		ID: "q", Data: "tok", From: telegram.User{ID: 4242},
		Message: &telegram.Message{MessageID: 3, Chat: telegram.Chat{ID: 4242, Type: "private"}},
	}}
	bot := &scriptedBot{steps: []pollStep{{updates: []telegram.Update{privateText(60, 1, "one"), voice, button}}}, onDone: cancel}

	var log []string
	sink := &fakePoster{log: &log}
	att := fakeAttacher{log: &log}
	if err := runPoll(t, ctx, bot, att, sink, path, clock.NewFake(time.Unix(0, 0))); err != nil {
		t.Fatalf("Poll: %v", err)
	}
	ev1, _, _ := Inbound(privateText(60, 1, "one"))
	ev2, _, _ := Inbound(voice)
	ev3, _, _ := InboundCallback(*button.CallbackQuery)
	want := []string{
		"post:" + *ev1.EventID,
		"attach:" + *ev2.EventID, "post:" + *ev2.EventID,
		"post:" + *ev3.EventID,
	}
	if !reflect.DeepEqual(log, want) {
		t.Fatalf("call order:\n got %v\nwant %v", log, want)
	}
	if got := config.LoadOffset(path); got != 63 {
		t.Fatalf("offset = %d, want 63", got)
	}
}

func TestPollLoopTransientAttachErrorDoesNotAdvance(t *testing.T) {
	path := offsetFile(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	voice := privateText(70, 1, "")
	voice.Message.Voice = &telegram.Voice{FileID: "v1"}
	clk := clock.NewFake(time.Unix(0, 0))
	bot := &scriptedBot{steps: []pollStep{{updates: []telegram.Update{voice}}}, called: make(chan struct{}, 8)}
	sink := &fakePoster{}
	done := make(chan error, 1)
	go func() {
		done <- Poll(ctx, bot, fakeAttacher{err: errors.New("upload 503")}, sink, path, clk)
	}()
	<-bot.called
	clk.BlockUntil(1) // backing off before it polls the same offset again
	cancel()
	if err := <-done; err != nil {
		t.Fatalf("Poll: %v", err)
	}
	if len(sink.ids()) != 0 {
		t.Fatalf("event posted despite failed attach: %v", sink.ids())
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatalf("offset written after a failed attach (%v)", err)
	}
}

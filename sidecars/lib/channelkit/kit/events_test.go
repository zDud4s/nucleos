package kit

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
)

// scripted answers one response per request, in order; the last one repeats.
type scripted struct {
	mu      sync.Mutex
	replies []reply
	ids     []string
	routes  []string
}

type reply struct {
	status int
	body   string
}

func (s *scripted) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	raw, _ := io.ReadAll(r.Body)
	var ev struct {
		EventID string `json:"event_id"`
	}
	_ = json.Unmarshal(raw, &ev)
	s.mu.Lock()
	n := len(s.ids)
	s.ids = append(s.ids, ev.EventID)
	s.routes = append(s.routes, r.Method+" "+r.URL.Path)
	rp := s.replies[min(n, len(s.replies)-1)]
	s.mu.Unlock()
	w.WriteHeader(rp.status)
	_, _ = w.Write([]byte(rp.body))
}

func (s *scripted) seen() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]string(nil), s.ids...)
}

func (s *scripted) firstRoute() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.routes[0]
}

func newSink(t *testing.T, replies ...reply) (*Sink, *scripted, *Stats, *clock.Fake) {
	t.Helper()
	sc := &scripted{replies: replies}
	srv := httptest.NewServer(sc)
	t.Cleanup(srv.Close)
	clk := clock.NewFake(time.Date(2026, 10, 19, 10, 0, 0, 0, time.UTC))
	c := NewClient(Config{BaseURL: srv.URL, Channel: "tg", Token: testToken, Clock: clk})
	stats := &Stats{}
	return NewSink(c, stats), sc, stats, clk
}

func sinkEvent(t *testing.T, id string) contract.Event {
	t.Helper()
	body := `{"event_id":"` + id + `","conversation":{"ref":"4242","kind":"private"},"sender":{"ref":"4242"},` +
		`"type":"message","message":{"ref":"992","parts":[{"type":"text","text":"hi"}]}}`
	ev, err := contract.DecodeStrict[contract.Event]([]byte(body))
	if err != nil {
		t.Fatal(err)
	}
	return ev
}

type postResult struct {
	out Outcome
	err error
}

func postAsync(s *Sink, ctx context.Context, ev contract.Event) <-chan postResult {
	ch := make(chan postResult, 1)
	go func() {
		out, err := s.Post(ctx, ev)
		ch <- postResult{out, err}
	}()
	return ch
}

func waitResult(t *testing.T, ch <-chan postResult) postResult {
	t.Helper()
	select {
	case r := <-ch:
		return r
	case <-time.After(5 * time.Second):
		t.Fatal("Post did not return")
		return postResult{}
	}
}

func TestSinkAdvancesOnAcceptedDuplicateAndDropped(t *testing.T) {
	for name, rp := range map[string]reply{
		"accepted":  {202, `{"accepted":true}`},
		"duplicate": {200, `{"duplicate":true}`},
		"dropped":   {202, `{"dropped":true}`},
	} {
		t.Run(name, func(t *testing.T) {
			s, sc, stats, _ := newSink(t, rp)
			out, err := s.Post(context.Background(), sinkEvent(t, "upd-1"))
			if err != nil || !out.Advance {
				t.Fatalf("out=%+v err=%v", out, err)
			}
			if got := sc.seen(); len(got) != 1 || got[0] != "upd-1" {
				t.Errorf("requests = %v", got)
			}
			if r := sc.firstRoute(); r != "POST /channels/tg/events" {
				t.Errorf("route = %q", r)
			}
			if stats.Snapshot().PermanentFailures != 0 {
				t.Error("a success must not count a failure")
			}
		})
	}
}

func TestSinkPermanentRejectionAdvancesAndCountsFailure(t *testing.T) {
	raw, err := fs.ReadFile(fixtures.FS(), "errors/bad-request.json")
	if err != nil {
		t.Fatal(err)
	}
	for _, status := range []int{400, 413, 422} {
		s, sc, stats, _ := newSink(t, reply{status, string(raw)})
		out, err := s.Post(context.Background(), sinkEvent(t, "upd-2"))
		if err != nil || !out.Advance {
			t.Fatalf("status %d: out=%+v err=%v", status, out, err)
		}
		if n := len(sc.seen()); n != 1 {
			t.Errorf("status %d: %d requests, want 1 (no retry)", status, n)
		}
		if got := stats.Snapshot().PermanentFailures; got != 1 {
			t.Errorf("status %d: PermanentFailures = %d", status, got)
		}
	}
}

func TestSinkRetriesServerErrorWithGrowingBackoffAndSameEventID(t *testing.T) {
	s, sc, stats, clk := newSink(t, reply{500, ""}, reply{500, ""}, reply{202, `{"accepted":true}`})
	ch := postAsync(s, context.Background(), sinkEvent(t, "upd-3"))

	clk.BlockUntil(1) // first wait: 2s
	clk.Advance(time.Second)
	if clk.Pending() != 1 {
		t.Fatal("the 2s wait ended after 1s")
	}
	clk.Advance(time.Second)

	clk.BlockUntil(1) // second wait: 4s
	clk.Advance(3 * time.Second)
	if clk.Pending() != 1 {
		t.Fatal("the 4s wait ended after 3s")
	}
	if n := len(sc.seen()); n != 2 {
		t.Fatalf("%d requests before the second wait ends, want 2", n)
	}
	clk.Advance(time.Second)

	r := waitResult(t, ch)
	if r.err != nil || !r.out.Advance {
		t.Fatalf("out=%+v err=%v", r.out, r.err)
	}
	ids := sc.seen()
	if len(ids) != 3 || ids[0] != "upd-3" || ids[1] != "upd-3" || ids[2] != "upd-3" {
		t.Errorf("event ids = %v, want the same one three times", ids)
	}
	if stats.Snapshot().PermanentFailures != 0 {
		t.Error("a retried success must not count a failure")
	}
}

func TestSinkRetriesTooManyRequests(t *testing.T) {
	s, sc, _, clk := newSink(t, reply{429, `{"error":"rate_limited"}`}, reply{202, `{"accepted":true}`})
	ch := postAsync(s, context.Background(), sinkEvent(t, "upd-4"))
	clk.BlockUntil(1)
	clk.Advance(2 * time.Second)
	r := waitResult(t, ch)
	if r.err != nil || !r.out.Advance || len(sc.seen()) != 2 {
		t.Fatalf("out=%+v err=%v requests=%d", r.out, r.err, len(sc.seen()))
	}
}

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func TestSinkRetriesNetworkError(t *testing.T) {
	var mu sync.Mutex
	var ids []string
	calls := 0
	rt := roundTripFunc(func(r *http.Request) (*http.Response, error) {
		raw, _ := io.ReadAll(r.Body)
		var ev struct {
			EventID string `json:"event_id"`
		}
		_ = json.Unmarshal(raw, &ev)
		mu.Lock()
		defer mu.Unlock()
		ids = append(ids, ev.EventID)
		calls++
		if calls == 1 {
			return nil, errors.New("connection refused")
		}
		return &http.Response{StatusCode: 202, Body: io.NopCloser(strings.NewReader(`{"accepted":true}`)), Header: http.Header{}}, nil
	})
	clk := clock.NewFake(time.Date(2026, 10, 19, 10, 0, 0, 0, time.UTC))
	c := NewClient(Config{BaseURL: "http://core.invalid", Channel: "tg", Token: testToken, Clock: clk, HTTP: &http.Client{Transport: rt}})
	s := NewSink(c, &Stats{})
	ch := postAsync(s, context.Background(), sinkEvent(t, "upd-5"))
	clk.BlockUntil(1)
	clk.Advance(2 * time.Second)
	r := waitResult(t, ch)
	if r.err != nil || !r.out.Advance {
		t.Fatalf("out=%+v err=%v", r.out, r.err)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(ids) != 2 || ids[0] != "upd-5" || ids[1] != "upd-5" {
		t.Errorf("event ids = %v", ids)
	}
}

func TestSinkContextCancelledDuringRetryDoesNotAdvance(t *testing.T) {
	s, _, _, clk := newSink(t, reply{500, ""})
	ctx, cancel := context.WithCancel(context.Background())
	ch := postAsync(s, ctx, sinkEvent(t, "upd-6"))
	clk.BlockUntil(1)
	cancel()
	r := waitResult(t, ch)
	if !errors.Is(r.err, context.Canceled) || r.out.Advance {
		t.Fatalf("out=%+v err=%v", r.out, r.err)
	}
}

func TestSinkContextCancelledBeforeSendDoesNotAdvance(t *testing.T) {
	s, sc, _, _ := newSink(t, reply{202, `{"accepted":true}`})
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	out, err := s.Post(ctx, sinkEvent(t, "upd-7"))
	if !errors.Is(err, context.Canceled) || out.Advance {
		t.Fatalf("out=%+v err=%v", out, err)
	}
	if len(sc.seen()) != 0 {
		t.Error("a cancelled context must not send")
	}
}

func TestSinkFatalStatusesAreNotRetried(t *testing.T) {
	for _, status := range []int{401, 403, 409} {
		s, sc, stats, _ := newSink(t, reply{status, `{"error":"channel_mismatch"}`})
		out, err := s.Post(context.Background(), sinkEvent(t, "upd-8"))
		if !errors.Is(err, ErrFatal) || out.Advance {
			t.Fatalf("status %d: out=%+v err=%v", status, out, err)
		}
		if n := len(sc.seen()); n != 1 {
			t.Errorf("status %d: %d requests, want 1", status, n)
		}
		if stats.Snapshot().PermanentFailures != 0 {
			t.Errorf("status %d: a fatal error is not a skipped event", status)
		}
	}
}

func TestSinkLocallyInvalidEventAdvancesAndIsNeverSent(t *testing.T) {
	s, sc, stats, _ := newSink(t, reply{202, `{"accepted":true}`})
	bad := sinkEvent(t, "upd-9")
	empty := ""
	bad.EventID = &empty
	out, err := s.Post(context.Background(), bad)
	if err != nil || !out.Advance {
		t.Fatalf("out=%+v err=%v", out, err)
	}
	if len(sc.seen()) != 0 {
		t.Error("an invalid event reached the core")
	}
	if got := stats.Snapshot().PermanentFailures; got != 1 {
		t.Errorf("PermanentFailures = %d", got)
	}
}

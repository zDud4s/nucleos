package kit

import (
	"context"
	"errors"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
)

func ackRespFixture(t *testing.T, name string) string {
	t.Helper()
	raw, err := fs.ReadFile(fixtures.FS(), "acks/"+name)
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

func newAckClient(t *testing.T, replies ...reply) (*Client, *scripted, *Stats, *clock.Fake) {
	t.Helper()
	sc := &scripted{replies: replies}
	srv := httptest.NewServer(sc)
	t.Cleanup(srv.Close)
	clk := clock.NewFake(time.Date(2026, 10, 19, 10, 0, 0, 0, time.UTC))
	stats := &Stats{}
	c := NewClient(Config{BaseURL: srv.URL, Channel: "tg", Token: testToken, Clock: clk}).WithStats(stats)
	return c, sc, stats, clk
}

func okAck() contract.Ack { return contract.Ack{OK: true, PlatformRefs: []string{"1"}} }

func TestAckClientResponses(t *testing.T) {
	cases := []struct {
		file string
		want AckResult
		late int64
	}{
		{"response-done.json", AckResult{State: contract.AckDone}, 0},
		{"response-pending.json", AckResult{State: contract.AckPending}, 0},
		{"response-ignored.json", AckResult{State: contract.AckFailed, Ignored: true}, 1},
		{"response-done-ignored.json", AckResult{State: contract.AckDone, Ignored: true}, 1},
	}
	for _, tc := range cases {
		t.Run(tc.file, func(t *testing.T) {
			var logged []string
			old := logf
			logf = func(f string, a ...any) { logged = append(logged, f) }
			t.Cleanup(func() { logf = old })
			c, sc, stats, _ := newAckClient(t, reply{200, ackRespFixture(t, tc.file)})
			got, err := c.Ack(context.Background(), 7, okAck())
			if err != nil {
				t.Fatalf("Ack: %v", err)
			}
			if got != tc.want {
				t.Fatalf("got %+v, want %+v", got, tc.want)
			}
			if r := sc.firstRoute(); r != "POST /channels/tg/outbox/7/ack" {
				t.Fatalf("route %q", r)
			}
			if len(sc.seen()) != 1 || len(logged) != 0 {
				t.Fatalf("requests %d, logs %v", len(sc.seen()), logged)
			}
			if s := stats.Snapshot(); s.LateAcks != tc.late || s.AckRejected != 0 {
				t.Fatalf("stats %+v", s)
			}
		})
	}
}

func TestAckClientRetryThenSuccess(t *testing.T) {
	c, sc, _, clk := newAckClient(t, reply{500, ""}, reply{500, ""}, reply{200, ackRespFixture(t, "response-done.json")})
	type out struct {
		res AckResult
		err error
	}
	done := make(chan out, 1)
	go func() {
		r, err := c.AckWithRetry(context.Background(), 3, okAck())
		done <- out{r, err}
	}()
	clk.BlockUntil(1)
	if n := len(sc.seen()); n != 1 {
		t.Fatalf("after first failure: %d requests", n)
	}
	clk.Advance(2 * time.Second)
	clk.BlockUntil(1)
	if n := len(sc.seen()); n != 2 {
		t.Fatalf("after second failure: %d requests", n)
	}
	clk.Advance(4 * time.Second)
	select {
	case o := <-done:
		if o.err != nil || o.res.State != contract.AckDone {
			t.Fatalf("got %+v, %v", o.res, o.err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("AckWithRetry did not return")
	}
	if n := len(sc.seen()); n != 3 {
		t.Fatalf("%d requests, want 3", n)
	}
}

func TestAckClientNotLeasedIsLost(t *testing.T) {
	c, sc, _, _ := newAckClient(t, reply{409, string(errorFixture(t, "not-leased.json"))})
	res, err := c.AckWithRetry(context.Background(), 3, okAck())
	if err != nil || res.State != AckStateLost {
		t.Fatalf("got %+v, %v", res, err)
	}
	if n := len(sc.seen()); n != 1 {
		t.Fatalf("%d requests", n)
	}
}

func TestAckClientRejectedIsNotRetried(t *testing.T) {
	for _, status := range []int{400, 404} {
		c, sc, stats, _ := newAckClient(t, reply{status, `{"error":"bad_request"}`})
		_, err := c.AckWithRetry(context.Background(), 3, okAck())
		if !errors.Is(err, ErrAckRejected) {
			t.Fatalf("status %d: err %v", status, err)
		}
		if n := len(sc.seen()); n != 1 {
			t.Fatalf("status %d: %d requests", status, n)
		}
		if got := stats.Snapshot().AckRejected; got != 1 {
			t.Fatalf("status %d: AckRejected %d", status, got)
		}
	}
}

func TestAckClientFatalStatuses(t *testing.T) {
	for _, status := range []int{401, 403} {
		c, sc, _, _ := newAckClient(t, reply{status, `{"error":"channel_mismatch"}`})
		_, err := c.AckWithRetry(context.Background(), 3, okAck())
		if !errors.Is(err, ErrFatal) {
			t.Fatalf("status %d: err %v", status, err)
		}
		if n := len(sc.seen()); n != 1 {
			t.Fatalf("status %d: %d requests", status, n)
		}
	}
}

func TestAckClientRetriesTooManyRequestsUntilContextEnds(t *testing.T) {
	c, sc, _, clk := newAckClient(t, reply{429, ""})
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		_, err := c.AckWithRetry(ctx, 3, okAck())
		done <- err
	}()
	clk.BlockUntil(1)
	clk.Advance(2 * time.Second)
	clk.BlockUntil(1)
	if n := len(sc.seen()); n != 2 {
		t.Fatalf("%d requests, want 2", n)
	}
	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("err %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("AckWithRetry did not return")
	}
}

// renewServer answers renew requests with respond(n), n counting from 1.
type renewServer struct {
	mu      sync.Mutex
	n       int
	routes  []string
	respond func(n int) (int, string)
}

func (s *renewServer) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	s.mu.Lock()
	s.n++
	n := s.n
	s.routes = append(s.routes, r.Method+" "+r.URL.Path)
	s.mu.Unlock()
	status, body := s.respond(n)
	if status == 0 { // simulate a dropped connection
		hj, _ := w.(http.Hijacker)
		conn, _, _ := hj.Hijack()
		_ = conn.Close()
		return
	}
	w.WriteHeader(status)
	_, _ = w.Write([]byte(body))
}

func (s *renewServer) count() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.n
}

func (s *renewServer) firstRoute() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.routes[0]
}

const renewOK = `{"lease_until":"2026-10-19T10:01:00Z"}`

func newRenewer(t *testing.T, respond func(n int) (int, string)) (*Renewer, *renewServer, *clock.Fake) {
	t.Helper()
	rs := &renewServer{respond: respond}
	srv := httptest.NewServer(rs)
	t.Cleanup(srv.Close)
	clk := clock.NewFake(time.Date(2026, 10, 19, 10, 0, 0, 0, time.UTC))
	c := NewClient(Config{BaseURL: srv.URL, Channel: "tg", Token: testToken, Clock: clk})
	return NewRenewer(c), rs, clk
}

// waitCount waits until the server has seen n renew requests.
func waitCount(t *testing.T, rs *renewServer, n int) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for rs.count() < n {
		if time.Now().After(deadline) {
			t.Fatalf("only %d requests, want %d", rs.count(), n)
		}
		time.Sleep(time.Millisecond)
	}
}

func TestRenewerRenewsEveryThirtySeconds(t *testing.T) {
	r, rs, clk := newRenewer(t, func(int) (int, string) { return 200, renewOK })
	lease := r.Start(context.Background(), 9)
	for i := 1; i <= 3; i++ {
		clk.BlockUntil(1)
		clk.Advance(29 * time.Second)
		if rs.count() != i-1 {
			t.Fatalf("renewed early: %d requests", rs.count())
		}
		clk.Advance(time.Second)
		waitCount(t, rs, i)
	}
	lease.Stop()
	if got := rs.firstRoute(); got != "POST /channels/tg/outbox/9/renew" {
		t.Fatalf("route %q", got)
	}
}

func TestRenewerStopsOnNotLeasedAndResumes(t *testing.T) {
	r, rs, clk := newRenewer(t, func(n int) (int, string) {
		if n == 1 {
			return 409, `{"error":"not_leased"}`
		}
		return 200, renewOK
	})
	lease := r.Start(context.Background(), 9)
	lost := lease.Lost()
	clk.BlockUntil(1)
	clk.Advance(30 * time.Second)
	select {
	case <-lost:
	case <-time.After(5 * time.Second):
		t.Fatal("Lost was not signalled")
	}
	if clk.Pending() != 0 {
		t.Fatal("a timer is still armed after the lease was lost")
	}
	clk.Advance(5 * time.Minute)
	if rs.count() != 1 {
		t.Fatalf("%d requests after a single 409, want 1", rs.count())
	}
	lease.Resume()
	next := lease.Lost()
	select {
	case <-next:
		t.Fatal("fresh Lost channel already closed")
	default:
	}
	clk.BlockUntil(1)
	clk.Advance(30 * time.Second)
	waitCount(t, rs, 2)
	lease.Resume() // active: no-op, no second goroutine
	lease.Stop()
}

func TestRenewerKeepsTryingAfterNetworkError(t *testing.T) {
	r, rs, clk := newRenewer(t, func(n int) (int, string) {
		if n == 1 {
			return 0, ""
		}
		return 200, renewOK
	})
	lease := r.Start(context.Background(), 9)
	clk.BlockUntil(1)
	clk.Advance(30 * time.Second)
	waitCount(t, rs, 1)
	clk.BlockUntil(1)
	clk.Advance(30 * time.Second)
	waitCount(t, rs, 2)
	select {
	case <-lease.Lost():
		t.Fatal("a network error must not lose the lease")
	default:
	}
	lease.Stop()
}

func TestRenewerStopBlocksUntilGoroutineExits(t *testing.T) {
	r, rs, clk := newRenewer(t, func(int) (int, string) { return 200, renewOK })
	lease := r.Start(context.Background(), 9)
	clk.BlockUntil(1)
	lease.Stop() // returns only after the goroutine exited
	if clk.Pending() != 0 {
		t.Fatal("timer left armed after Stop")
	}
	clk.Advance(time.Minute)
	if rs.count() != 0 {
		t.Fatalf("%d renewals after Stop", rs.count())
	}
	lease.Resume() // after Stop: no-op
	if clk.Pending() != 0 {
		t.Fatal("Resume restarted a stopped lease")
	}
	lease.Stop() // idempotent
}

func TestRenewerContextCancelStops(t *testing.T) {
	r, rs, clk := newRenewer(t, func(int) (int, string) { return 200, renewOK })
	ctx, cancel := context.WithCancel(context.Background())
	lease := r.Start(ctx, 9)
	clk.BlockUntil(1)
	cancel()
	lease.Stop() // waits for the goroutine that cancellation ended
	clk.Advance(time.Minute)
	if rs.count() != 0 {
		t.Fatalf("%d renewals after cancel", rs.count())
	}
	select {
	case <-lease.Lost():
		t.Fatal("cancellation is not a lost lease")
	default:
	}
}

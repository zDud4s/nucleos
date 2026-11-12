package serve

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"
)

func newTestLimiterSet(limit int, window time.Duration, now *time.Time) *limiterSet {
	s := newLimiterSet(limit, window)
	s.now = func() time.Time { return *now }
	return s
}

// limitRig runs the real auth handler in front of the fake passkey daemon, with one fake clock
// shared by every limiter so a test can move time without sleeping.
type limitRig struct {
	pd    *passkeyDaemon
	ah    *authHandler
	gw    *httptest.Server
	calls *atomic.Int32 // requests that reached the daemon
	clock *atomic.Int64 // unix nanoseconds
	t     *testing.T
}

type limitResp struct {
	status int
	retry  string
	body   string
	scope  string
}

func newLimitRig(t *testing.T) *limitRig {
	t.Helper()
	pd := newPasskeyDaemon()
	h := pd.handler(t)
	calls := &atomic.Int32{}
	srv := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		h(w, r)
	})
	cfg := g2Config(srv.URL)
	ah := NewAuth(cfg).(*authHandler)
	api, err := NewAPI(cfg)
	if err != nil {
		t.Fatal(err)
	}
	gw := httptest.NewServer(Build(cfg, Handlers{Auth: ah, API: api, Static: NewStatic(cfg)}))
	t.Cleanup(gw.Close)
	clock := &atomic.Int64{}
	clock.Store(time.Unix(1000, 0).UnixNano())
	fake := func() time.Time { return time.Unix(0, clock.Load()) }
	ah.pglobal.now, ah.pdev.now, ah.pfail.now = fake, fake, fake
	return &limitRig{pd: pd, ah: ah, gw: gw, calls: calls, clock: clock, t: t}
}

func (r *limitRig) advance(d time.Duration) { r.clock.Add(int64(d)) }

func (r *limitRig) do(method, path, body, session string) limitResp {
	r.t.Helper()
	hdr := map[string]string{}
	if session != "" {
		hdr = g2Cookie(session)
	}
	resp := g2Do(r.t, r.gw, method, path, body, hdr)
	out := limitResp{status: resp.StatusCode, retry: resp.Header.Get("Retry-After"), body: g2Body(r.t, resp)}
	var parsed map[string]any
	if json.Unmarshal([]byte(out.body), &parsed) == nil {
		out.scope, _ = parsed["scope"].(string)
	}
	return out
}

func (r *limitRig) status(session string) limitResp {
	r.t.Helper()
	return r.do("GET", "/auth/passkey/status", "", session)
}

// failFinish is a verify/finish with no ceremony behind it: a failed finish for the device.
func (r *limitRig) failFinish(session string) limitResp {
	r.t.Helper()
	return r.do("POST", "/auth/passkey/verify/finish", "{}", session)
}

func (r *limitRig) devices(n int) []string {
	sessions := make([]string, n)
	for i := range sessions {
		sessions[i] = fmt.Sprintf("nos_device_LIM%04d", i)
		r.pd.addDevice(sessions[i], int64(i+1), true)
	}
	return sessions
}

func (r *limitRig) deviceBuckets() int {
	r.ah.pdev.mu.Lock()
	defer r.ah.pdev.mu.Unlock()
	return len(r.ah.pdev.m)
}

func TestPasskeyLimitIsPerDevice(t *testing.T) {
	rig := newLimitRig(t)
	rig.pd.addDevice(sessA, 7, true)
	rig.pd.addDevice(sessB, 8, true)
	for i := 0; i < passkeyDeviceLimit; i++ {
		if got := rig.status(sessA); got.status != 200 {
			t.Fatalf("device A request %d = %d %s", i+1, got.status, got.body)
		}
	}
	got := rig.status(sessA)
	if got.status != 429 || got.scope != "device" || got.retry == "" || got.retry == "0" {
		t.Fatalf("device A over the limit = %d scope=%q retry=%q", got.status, got.scope, got.retry)
	}
	if b := rig.status(sessB); b.status != 200 {
		t.Fatalf("device B was limited by device A: %d %s", b.status, b.body)
	}
}

func TestPasskeyGlobalCeilingBacksStopsManyDevices(t *testing.T) {
	rig := newLimitRig(t)
	sessions := rig.devices(8)
	sent := 0
	for sent < passkeyGlobalLimit {
		// 8 devices share the ceiling and none gets near its own limit.
		if got := rig.status(sessions[sent%len(sessions)]); got.status != 200 {
			t.Fatalf("request %d = %d scope=%q %s", sent+1, got.status, got.scope, got.body)
		}
		sent++
	}
	before := rig.calls.Load()
	got := rig.status(sessions[0])
	if got.status != 429 || got.scope != "global" {
		t.Fatalf("request past the ceiling = %d scope=%q %s", got.status, got.scope, got.body)
	}
	if rig.calls.Load() != before {
		t.Fatal("a globally refused request still reached the daemon")
	}
}

func TestPasskeyInvalidCookieCountsOnlyGlobally(t *testing.T) {
	rig := newLimitRig(t)
	rig.pd.addDevice(sessA, 7, true)
	for i := 0; i < 3*passkeyDeviceLimit; i++ {
		got := rig.status(fmt.Sprintf("nos_device_BAD%04d", i))
		if got.status != 401 {
			t.Fatalf("bad cookie %d = %d %s", i, got.status, got.body)
		}
	}
	// One bad cookie repeated is no different: it never earns a device-scoped answer.
	for i := 0; i < passkeyDeviceLimit+5; i++ {
		if got := rig.status("nos_device_REPEATED"); got.status != 401 {
			t.Fatalf("repeated bad cookie = %d scope=%q", got.status, got.scope)
		}
	}
	if n := rig.deviceBuckets(); n != 0 {
		t.Fatalf("invalid cookies created %d per-device buckets", n)
	}
	if got := rig.status(sessA); got.status != 200 {
		t.Fatalf("a real device was locked out by invalid cookies: %d %s", got.status, got.body)
	}
}

func TestTicketRoutesKeyedByResolvedDevice(t *testing.T) {
	rig := newLimitRig(t)
	rig.pd.addDevice(sessA, 7, true)
	rig.pd.addDevice(sessB, 8, true)
	rig.pd.addTicket("tick-a1", 7, "label", "111")
	rig.pd.addTicket("tick-a2", 7, "label", "222")
	rig.pd.addTicket("tick-b1", 8, "label", "333")
	info := func(ticket string) limitResp {
		return rig.do("GET", "/auth/stepup/info?ticket="+ticket, "", "")
	}
	for i := 0; i < passkeyDeviceLimit; i++ {
		if got := info("nope" + fmt.Sprint(i)); got.status != 410 {
			t.Fatalf("unknown ticket = %d %s", got.status, got.body)
		}
	}
	if n := rig.deviceBuckets(); n != 0 {
		t.Fatalf("unknown tickets created %d per-device buckets", n)
	}
	// Two tickets of device 7 share one bucket of passkeyDeviceLimit.
	for i := 0; i < passkeyDeviceLimit; i++ {
		ticket := "tick-a1"
		if i%2 == 1 {
			ticket = "tick-a2"
		}
		if got := info(ticket); got.status != 200 {
			t.Fatalf("ticket request %d = %d %s", i+1, got.status, got.body)
		}
	}
	got := info("tick-a1")
	if got.status != 429 || got.scope != "device" {
		t.Fatalf("second ticket of a limited device = %d scope=%q", got.status, got.scope)
	}
	if got := info("tick-a2"); got.status != 429 || got.scope != "device" {
		t.Fatalf("tickets of one device did not share a bucket: %d scope=%q", got.status, got.scope)
	}
	if got := info("tick-b1"); got.status != 200 {
		t.Fatalf("another device's ticket was limited: %d %s", got.status, got.body)
	}
	if n := rig.deviceBuckets(); n != 2 {
		t.Fatalf("buckets = %d, want one per resolved device (2)", n)
	}
}

func TestPasskeyFailureBrakeAndSuccessReset(t *testing.T) {
	rig := newLimitRig(t)
	rig.pd.addDevice(sessA, 7, true)
	rig.pd.addDevice(sessB, 8, true)
	auth := newVirtualAuthenticator(testOrigin)
	pr := &passkeyRig{daemon: rig.pd, gw: func(method, path, body, session string) (int, string) {
		got := rig.do(method, path, body, session)
		return got.status, got.body
	}}
	pr.enrol(t, sessA, auth)

	// Failures spread over several request windows, so only the failure brake can stop them.
	fail := func(n int) {
		t.Helper()
		for i := 0; i < n; i++ {
			if got := rig.failFinish(sessA); got.status != 400 {
				t.Fatalf("failed finish = %d scope=%q %s", got.status, got.scope, got.body)
			}
		}
		rig.advance(61 * time.Second)
	}
	fail(passkeyFailLimit - 1)
	if status, body := pr.verify(t, sessA, auth, true); status != 200 {
		t.Fatalf("verify = %d %s", status, body)
	}
	rig.advance(61 * time.Second)
	// The success cleared the count: another passkeyFailLimit-1 failures are still allowed.
	fail(passkeyFailLimit - 1)

	// One more failure engages the brake: 8 since the success.
	if got := rig.failFinish(sessA); got.status != 400 {
		t.Fatalf("failure %d = %d", passkeyFailLimit, got.status)
	}
	rig.advance(61 * time.Second)
	got := rig.status(sessA)
	if got.status != 429 || got.scope != "device" {
		t.Fatalf("braked device = %d scope=%q %s", got.status, got.scope, got.body)
	}
	if b := rig.status(sessB); b.status != 200 {
		t.Fatalf("the brake leaked to another device: %d", b.status)
	}
	// The brake lasts its whole window, then lifts.
	rig.advance(10 * time.Minute)
	if got := rig.status(sessA); got.status != 200 {
		t.Fatalf("brake did not lift after its window: %d %s", got.status, got.body)
	}
}

func TestLimiterSetSweepsAndCaps(t *testing.T) {
	now := time.Unix(100, 0)
	s := newTestLimiterSet(1, time.Minute, &now)
	if ok, _, _ := s.allow(1); !ok {
		t.Fatal("first bucket denied")
	}
	now = now.Add(time.Minute)
	if ok, _, _ := s.allow(2); !ok || len(s.m) != 1 {
		t.Fatalf("expired bucket was not swept: ok=%v buckets=%d", ok, len(s.m))
	}
	for id := int64(3); id < int64(limiterSetCap+3); id++ {
		if ok, _, _ := s.allow(id); !ok {
			t.Fatalf("bucket %d denied", id)
		}
	}
	if len(s.m) != limiterSetCap {
		t.Fatalf("buckets = %d, want %d", len(s.m), limiterSetCap)
	}
	if ok, _, _ := s.allow(999999); !ok || len(s.m) != limiterSetCap {
		t.Fatal("overflow key did not fall through without a bucket")
	}
}

func TestLimiterSetVerdictSurvivesThrottledSweep(t *testing.T) {
	now := time.Unix(100, 0)
	s := newTestLimiterSet(2, time.Minute, &now)
	s.allow(1)
	s.allow(1)
	if blocked, _, _ := s.blocked(1); !blocked {
		t.Fatal("full bucket not blocked")
	}
	now = now.Add(50 * time.Second)
	if blocked, _, _ := s.blocked(1); !blocked {
		t.Fatal("bucket released early")
	}
	// A sweep just ran, so the next one is not due for a quarter window: the events expire in
	// between and the verdict must still be right.
	now = now.Add(11 * time.Second)
	if blocked, _, _ := s.blocked(1); blocked {
		t.Fatal("expired events still block")
	}
}

func TestRetryAfterAndScopeBody(t *testing.T) {
	w := httptest.NewRecorder()
	tooManyRequests(w, 10*time.Millisecond, "device")
	if w.Code != http.StatusTooManyRequests || w.Header().Get("Retry-After") != "1" {
		t.Fatalf("status=%d retry=%q", w.Code, w.Header().Get("Retry-After"))
	}
	var body map[string]string
	if err := json.Unmarshal(w.Body.Bytes(), &body); err != nil {
		t.Fatal(err)
	}
	if body["error"] != "slow_down" || body["scope"] != "device" {
		t.Fatalf("body=%v", body)
	}
}

func (s *ceremonyStore) countFor(device int64) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	n := 0
	for _, c := range s.m {
		if c.deviceID == device {
			n++
		}
	}
	return n
}

func TestCeremonyEvictionStaysWithinDevice(t *testing.T) {
	s := newCeremonyStore()
	now := time.Unix(100, 0)
	s.now = func() time.Time { return now }
	for i := 0; i < 4; i++ {
		s.put(fmt.Sprintf("b%d", i), ceremony{deviceID: 2})
	}
	// Device A floods far past the global cap; the old store evicted random entries, B's included.
	for i := 0; i < 8*maxCeremonies; i++ {
		now = now.Add(time.Millisecond)
		s.put(fmt.Sprintf("a%d", i), ceremony{deviceID: 1})
	}
	for i := 0; i < 4; i++ {
		if _, ok := s.take(fmt.Sprintf("b%d", i)); !ok {
			t.Fatalf("device A's flood evicted device B's ceremony b%d", i)
		}
	}
	if n := s.countFor(1); n != maxCeremoniesPerDevice {
		t.Fatalf("device A holds %d ceremonies, want %d", n, maxCeremoniesPerDevice)
	}
	last := 8*maxCeremonies - 1
	if _, ok := s.take(fmt.Sprintf("a%d", last)); !ok {
		t.Fatal("A's newest ceremony was evicted")
	}
	if _, ok := s.take("a0"); ok {
		t.Fatal("A's oldest ceremony survived")
	}
}

func TestCeremonyGlobalCapIsTheLastResort(t *testing.T) {
	s := newCeremonyStore()
	now := time.Unix(100, 0)
	s.now = func() time.Time { return now }
	// Enough devices, each under its own cap, to exceed the global cap.
	for i := 0; i < 2*maxCeremonies; i++ {
		s.put(fmt.Sprintf("k%d", i), ceremony{deviceID: int64(i)})
	}
	s.mu.Lock()
	n := len(s.m)
	s.mu.Unlock()
	if n > maxCeremonies {
		t.Fatalf("store holds %d ceremonies, cap is %d", n, maxCeremonies)
	}
}

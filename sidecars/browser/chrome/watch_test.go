// §spec browser-ao-vivo

package chrome

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"image"
	"image/jpeg"
	"sync"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// The watch group. A viewer is a person looking at the agent's page through a screencast, and what
// these assert is the three promises the stream makes to the rest of the driver: it is paced by
// the ack (so a slow reader cannot flood the browser), it never stalls the connection's single
// dispatch goroutine (the fence answers on it), and a slow viewer sees the latest frame rather than
// a queue of old ones.

// watchSession opens one session on a fake and returns it, ready to be watched.
func watchSession(t *testing.T) (*cdptest.Browser, *Driver, browser.SessionID) {
	t.Helper()
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	session, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	return fake, driver, session.ID
}

// jpegAnswer makes the fake answer Page.captureScreenshot with these bytes.
func jpegAnswer(fake *cdptest.Browser, bytesToSend []byte) {
	fake.Handle("Page.captureScreenshot", func(cdptest.Call) (any, error) {
		return map[string]any{"data": base64.StdEncoding.EncodeToString(bytesToSend)}, nil
	})
}

// emitFrame raises one Page.screencastFrame on the page session.
func emitFrame(fake *cdptest.Browser, payload []byte, ackID int) {
	fake.Emit("S1", "Page.screencastFrame", map[string]any{
		"data":      base64.StdEncoding.EncodeToString(payload),
		"sessionId": ackID,
		"metadata":  map[string]any{},
	})
}

// collector is a sink that records what it was given.
type collector struct {
	mu     sync.Mutex
	frames [][]byte
}

func (c *collector) sink(frame browser.Frame) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.frames = append(c.frames, frame.JPEG)
}

func (c *collector) all() [][]byte {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([][]byte(nil), c.frames...)
}

// watching is one running Watch call.
type watching struct {
	cancel context.CancelFunc
	done   chan struct{}
}

func startWatch(t *testing.T, driver *Driver, id browser.SessionID, sink func(browser.Frame)) *watching {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	w := &watching{cancel: cancel, done: make(chan struct{})}
	go func() {
		_ = driver.Watch(ctx, id, sink)
		close(w.done)
	}()
	t.Cleanup(cancel)
	return w
}

// stop ends the watch and waits for Watch to return. Safe to call twice.
func (w *watching) stop(t *testing.T) {
	t.Helper()
	w.cancel()
	select {
	case <-w.done:
	case <-time.After(5 * time.Second):
	}
}

func eventually(t *testing.T, what string, within time.Duration, condition func() bool) {
	t.Helper()
	deadline := time.Now().Add(within)
	for time.Now().Before(deadline) {
		if condition() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

func countCalls(fake *cdptest.Browser, method string) int {
	n := 0
	for _, call := range fake.Calls() {
		if call.Method == method {
			n++
		}
	}
	return n
}

// ackLog records when each Page.screencastFrameAck arrived and which frame it acknowledged.
type ackLog struct {
	mu    sync.Mutex
	times []time.Time
	ids   []int
}

func recordAcks(fake *cdptest.Browser) *ackLog {
	log := &ackLog{}
	fake.Handle("Page.screencastFrameAck", func(call cdptest.Call) (any, error) {
		var params struct {
			SessionID int `json:"sessionId"`
		}
		_ = json.Unmarshal(call.Params, &params)
		log.mu.Lock()
		defer log.mu.Unlock()
		log.times = append(log.times, time.Now())
		log.ids = append(log.ids, params.SessionID)
		return nil, nil
	})
	return log
}

func (l *ackLog) count() int {
	l.mu.Lock()
	defer l.mu.Unlock()
	return len(l.ids)
}

func (l *ackLog) snapshot() ([]time.Time, []int) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]time.Time(nil), l.times...), append([]int(nil), l.ids...)
}

// TestWatchStartsAJpegScreencastCappedAt1280. The cap is what keeps a 4K monitor's page from
// streaming 4K frames to a viewer that shows them in a pane; the format and quality are what keep
// each frame small enough to ack at ten a second.
func TestWatchStartsAJpegScreencastCappedAt1280(t *testing.T) {
	fake, driver, id := watchSession(t)
	jpegAnswer(fake, []byte("shot"))
	var seen collector
	w := startWatch(t, driver, id, seen.sink)
	defer w.stop(t)

	eventually(t, "Page.startScreencast", 3*time.Second, func() bool { return countCalls(fake, "Page.startScreencast") > 0 })

	for _, call := range fake.Calls() {
		if call.Method != "Page.startScreencast" {
			continue
		}
		if call.Session != "S1" {
			t.Errorf("screencast started on session %q, want the page session S1", call.Session)
		}
		var params map[string]any
		if err := json.Unmarshal(call.Params, &params); err != nil {
			t.Fatalf("params: %v", err)
		}
		if params["format"] != "jpeg" {
			t.Errorf("format = %v, want jpeg", params["format"])
		}
		if params["quality"] != float64(60) {
			t.Errorf("quality = %v, want 60", params["quality"])
		}
		if params["maxWidth"] != float64(1280) || params["maxHeight"] != float64(1280) {
			t.Errorf("cap = %vx%v, want 1280x1280", params["maxWidth"], params["maxHeight"])
		}
		return
	}
}

// TestWatchAcksEachFrameNoSoonerThan100msAfterThePrevious. The ack is the only brake Chrome has on
// the stream; acking at once would run the page at full frame rate for a pane nobody can read that
// fast. The tolerance is 90 ms because the timestamps are taken on the fake's goroutine.
func TestWatchAcksEachFrameNoSoonerThan100msAfterThePrevious(t *testing.T) {
	fake, driver, id := watchSession(t)
	jpegAnswer(fake, []byte("shot"))
	acks := recordAcks(fake)
	var seen collector
	w := startWatch(t, driver, id, seen.sink)
	defer w.stop(t)
	eventually(t, "Page.startScreencast", 3*time.Second, func() bool { return countCalls(fake, "Page.startScreencast") > 0 })

	for n := 1; n <= 3; n++ {
		emitFrame(fake, []byte{byte('a' + n)}, n)
		want := n
		eventually(t, "an ack", 3*time.Second, func() bool { return acks.count() >= want })
	}

	times, ids := acks.snapshot()
	for i, acked := range ids[:3] {
		if acked != i+1 {
			t.Errorf("ack %d acknowledged frame %d, want %d", i, acked, i+1)
		}
	}
	for i := 1; i < 3; i++ {
		if gap := times[i].Sub(times[i-1]); gap < 90*time.Millisecond {
			t.Errorf("ack %d came %v after the previous one, want at least 100ms", i, gap)
		}
	}
}

// TestWatchFrameHandlerNeverWaitsForTheAck. Every handler runs serially on the connection's one
// dispatch goroutine, the fence's included, so a frame handler that slept out the pacing delay (or
// made a blocking call) would hold every paused request behind it — and a paused request nobody
// answers wedges the renderer. The assertion is ORDER: the fence's answer to a request paused right
// after the second frame comes before the second frame's ack.
func TestWatchFrameHandlerNeverWaitsForTheAck(t *testing.T) {
	fake, driver, id := watchSession(t)
	jpegAnswer(fake, []byte("shot"))
	acks := recordAcks(fake)
	var seen collector
	w := startWatch(t, driver, id, seen.sink)
	defer w.stop(t)
	eventually(t, "Page.startScreencast", 3*time.Second, func() bool { return countCalls(fake, "Page.startScreencast") > 0 })

	emitFrame(fake, []byte("one"), 1)
	eventually(t, "the first ack", 3*time.Second, func() bool { return acks.count() >= 1 })

	emitFrame(fake, []byte("two"), 2)
	pauseRequest(fake, "S1", requestStage("https://evil.example.net/page", "GET", "Document", nil))

	eventually(t, "the second ack", 3*time.Second, func() bool { return acks.count() >= 2 })
	waitForCall(t, fake, "Fetch.failRequest")

	methods := fake.Methods()
	failedAt, ackedTwiceAt, acksSeen := -1, -1, 0
	for i, method := range methods {
		switch method {
		case "Fetch.failRequest":
			if failedAt < 0 {
				failedAt = i
			}
		case "Page.screencastFrameAck":
			acksSeen++
			if acksSeen == 2 {
				ackedTwiceAt = i
			}
		}
	}
	if failedAt < 0 || ackedTwiceAt < 0 || failedAt > ackedTwiceAt {
		t.Fatalf("the fence's answer (%d) did not precede the second ack (%d): the frame handler waited: %v",
			failedAt, ackedTwiceAt, methods)
	}
}

// TestWatchSlowViewerGetsOnlyTheLatestFrame. A viewer that cannot keep up must not make the driver
// buffer, and must not be shown the past: when it comes back it gets the newest frame and nothing
// older, or the picture would play catch-up at the pace of the slowest reader.
func TestWatchSlowViewerGetsOnlyTheLatestFrame(t *testing.T) {
	fake, driver, id := watchSession(t)
	jpegAnswer(fake, []byte("shot"))
	acks := recordAcks(fake)

	var seen collector
	entered := make(chan struct{})
	release := make(chan struct{})
	var once sync.Once
	sink := func(frame browser.Frame) {
		seen.sink(frame)
		once.Do(func() { close(entered) })
		<-release
	}
	w := startWatch(t, driver, id, sink)
	defer w.stop(t)
	defer func() {
		select {
		case <-release:
		default:
			close(release)
		}
	}()

	select {
	case <-entered:
	case <-time.After(3 * time.Second):
		t.Fatal("the sink was never called")
	}
	eventually(t, "Page.startScreencast", 3*time.Second, func() bool { return countCalls(fake, "Page.startScreencast") > 0 })

	// The sink is now stuck inside its first call. Three more frames arrive while it is.
	emitFrame(fake, []byte("old-1"), 1)
	emitFrame(fake, []byte("old-2"), 2)
	emitFrame(fake, []byte("latest"), 3)
	eventually(t, "the last frame's ack", 5*time.Second, func() bool {
		_, ids := acks.snapshot()
		for _, acked := range ids {
			if acked == 3 {
				return true
			}
		}
		return false
	})

	close(release)
	eventually(t, "the latest frame", 3*time.Second, func() bool { return len(seen.all()) >= 2 })
	time.Sleep(300 * time.Millisecond)

	got := seen.all()
	if len(got) != 2 {
		t.Fatalf("the slow viewer was handed %d frames, want the one it was holding plus the latest: %q", len(got), got)
	}
	if !bytes.Equal(got[1], []byte("latest")) {
		t.Errorf("the frame after the stall was %q, want the latest", got[1])
	}
}

// TestWatchFirstFrameArrivesWithoutThePageChanging. A screencast sends a frame when the page paints,
// and a static page does not; a viewer that opened on one would stare at nothing until something
// moved. So every viewer is handed a captureScreenshot straight away.
func TestWatchFirstFrameArrivesWithoutThePageChanging(t *testing.T) {
	fake, driver, id := watchSession(t)
	jpegAnswer(fake, []byte("the-static-page"))
	var seen collector
	w := startWatch(t, driver, id, seen.sink)
	defer w.stop(t)

	eventually(t, "the first frame", 3*time.Second, func() bool { return len(seen.all()) >= 1 })
	if got := seen.all()[0]; !bytes.Equal(got, []byte("the-static-page")) {
		t.Errorf("first frame = %q, want the screenshot's bytes", got)
	}

	for _, call := range fake.Calls() {
		if call.Method != "Page.captureScreenshot" {
			continue
		}
		var params map[string]any
		if err := json.Unmarshal(call.Params, &params); err != nil {
			t.Fatalf("params: %v", err)
		}
		if params["format"] != "jpeg" || params["quality"] != float64(60) {
			t.Errorf("screenshot params = %v, want jpeg at quality 60", params)
		}
		return
	}
	t.Fatal("no Page.captureScreenshot was made")
}

// TestWatchAfterHandoffStreamsTheAgentsPage. A handoff marks the wheel as the person's, and the
// person's window is the one a viewer is waiting to see: the screencast keeps running over the same
// page, so the viewer follows the login instead of being cut off at the moment it matters most.
func TestWatchAfterHandoffStreamsTheAgentsPage(t *testing.T) {
	fake, driver, id := watchSession(t)
	jpegAnswer(fake, []byte("shot"))
	if _, err := driver.Handoff(context.Background(), id, "the person takes over"); err != nil {
		t.Fatalf("handoff: %v", err)
	}

	var seen collector
	w := startWatch(t, driver, id, seen.sink)
	defer w.stop(t)

	eventually(t, "a frame after the handoff", 3*time.Second, func() bool { return len(seen.all()) >= 1 })
	eventually(t, "Page.startScreencast", 3*time.Second, func() bool { return countCalls(fake, "Page.startScreencast") > 0 })
	emitFrame(fake, []byte("after-handoff"), 1)
	eventually(t, "the screencast's own frame", 3*time.Second, func() bool { return len(seen.all()) >= 2 })
	if got := seen.all()[1]; !bytes.Equal(got, []byte("after-handoff")) {
		t.Errorf("second frame = %q, want the one the screencast sent", got)
	}
}

// TestWatchLastViewerLeavingStopsTheScreencast. One screencast is shared by every viewer of a
// session: the second viewer must not start another, and only the last one leaving stops it — a
// screencast left running costs the page a frame encode on every paint for nobody.
func TestWatchLastViewerLeavingStopsTheScreencast(t *testing.T) {
	fake, driver, id := watchSession(t)
	jpegAnswer(fake, []byte("shot"))

	var first, second collector
	a := startWatch(t, driver, id, first.sink)
	defer a.stop(t)
	eventually(t, "the first viewer's frame", 3*time.Second, func() bool { return len(first.all()) >= 1 })
	b := startWatch(t, driver, id, second.sink)
	defer b.stop(t)
	eventually(t, "the second viewer's frame", 3*time.Second, func() bool { return len(second.all()) >= 1 })

	if got := countCalls(fake, "Page.startScreencast"); got != 1 {
		t.Errorf("Page.startScreencast was called %d times for two viewers, want 1", got)
	}
	if got := countCalls(fake, "Page.captureScreenshot"); got != 2 {
		t.Errorf("Page.captureScreenshot was called %d times, want one per viewer (2)", got)
	}

	a.stop(t)
	time.Sleep(200 * time.Millisecond)
	if got := countCalls(fake, "Page.stopScreencast"); got != 0 {
		t.Fatalf("the screencast was stopped with a viewer still watching (%d stop calls)", got)
	}

	b.stop(t)
	eventually(t, "Page.stopScreencast", 3*time.Second, func() bool { return countCalls(fake, "Page.stopScreencast") > 0 })
	if got := countCalls(fake, "Page.stopScreencast"); got != 1 {
		t.Errorf("Page.stopScreencast was called %d times, want 1", got)
	}
}

// TestWatchDeliversNoFrameOnceItsContextIsCancelled. A Go select picks at random between ready
// cases, so a viewer whose context is cancelled while a frame waits in its slot used to hand that
// frame to the sink about half the time. Each round cancels from inside the sink (the first frame is
// the screenshot), waits until a second frame sits in the viewer's slot, and then lets the loop
// choose: anything the sink is given after that is a frame shown after the end.
func TestWatchDeliversNoFrameOnceItsContextIsCancelled(t *testing.T) {
	fake, driver, id := watchSession(t)
	jpegAnswer(fake, []byte("shot"))
	fake.Handle("Page.screencastFrameAck", func(cdptest.Call) (any, error) { return nil, nil })

	slotFull := func() bool {
		driver.mu.Lock()
		defer driver.mu.Unlock()
		for _, cast := range driver.casts {
			for v := range cast.viewers {
				if len(v.slot) > 0 {
					return true
				}
			}
		}
		return false
	}

	const rounds = 200
	late := 0
	for round := 0; round < rounds; round++ {
		ctx, cancel := context.WithCancel(context.Background())
		var mu sync.Mutex
		calls := 0
		afterCancel := 0
		sink := func(browser.Frame) {
			mu.Lock()
			calls++
			first := calls == 1
			if !first {
				afterCancel++
			}
			mu.Unlock()
			if !first {
				return
			}
			emitFrame(fake, []byte("next"), round+1)
			deadline := time.Now().Add(3 * time.Second)
			for !slotFull() && time.Now().Before(deadline) {
				time.Sleep(time.Millisecond)
			}
			cancel()
		}
		finished := make(chan struct{})
		go func() {
			_ = driver.Watch(ctx, id, sink)
			close(finished)
		}()
		select {
		case <-finished:
		case <-time.After(10 * time.Second):
			cancel()
			t.Fatalf("round %d: Watch did not return after its context was cancelled", round)
		}
		cancel()
		mu.Lock()
		if afterCancel > 0 {
			late++
		}
		mu.Unlock()
	}
	if late > 0 {
		t.Errorf("%d of %d rounds handed the sink a frame after the context was cancelled; want none", late, rounds)
	}
}

// tinyJPEG encodes a real 4x3 JPEG, so the driver can read the frame's own dimensions from it.
func tinyJPEG(t *testing.T) []byte {
	t.Helper()
	var buf bytes.Buffer
	if err := jpeg.Encode(&buf, image.NewRGBA(image.Rect(0, 0, 4, 3)), nil); err != nil {
		t.Fatalf("encode: %v", err)
	}
	return buf.Bytes()
}

// frameLog records whole frames, metadata included.
type frameLog struct {
	mu     sync.Mutex
	frames []browser.Frame
}

func (l *frameLog) sink(frame browser.Frame) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.frames = append(l.frames, frame)
}

func (l *frameLog) all() []browser.Frame {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]browser.Frame(nil), l.frames...)
}

// TestWatchFrameCarriesItsScreencastMetadata. The viewer maps a click on the picture back to the
// page, and it can only do that with the metadata Chrome sends beside each screencast frame.
func TestWatchFrameCarriesItsScreencastMetadata(t *testing.T) {
	fake, driver, id := watchSession(t)
	shot := tinyJPEG(t)
	jpegAnswer(fake, shot)
	var seen frameLog
	w := startWatch(t, driver, id, seen.sink)
	defer w.stop(t)
	eventually(t, "Page.startScreencast", 3*time.Second, func() bool { return countCalls(fake, "Page.startScreencast") > 0 })
	eventually(t, "the first screenshot", 3*time.Second, func() bool { return len(seen.all()) >= 1 })

	fake.Emit("S1", "Page.screencastFrame", map[string]any{
		"data":      base64.StdEncoding.EncodeToString(shot),
		"sessionId": 1,
		"metadata": map[string]any{
			"offsetTop":       56,
			"pageScaleFactor": 1.5,
			"deviceWidth":     800,
			"deviceHeight":    600,
			"scrollOffsetX":   10,
			"scrollOffsetY":   20,
		},
	})
	eventually(t, "the screencast's own frame", 3*time.Second, func() bool { return len(seen.all()) >= 2 })

	got := seen.all()[1]
	want := browser.FrameMeta{
		FrameWidth: 4, FrameHeight: 3,
		DeviceWidth: 800, DeviceHeight: 600,
		OffsetTop: 56, PageScaleFactor: 1.5,
		ScrollOffsetX: 10, ScrollOffsetY: 20,
	}
	if got.Meta == nil {
		t.Fatal("the screencast frame carried no metadata")
	}
	if *got.Meta != want {
		t.Errorf("meta = %+v, want %+v", *got.Meta, want)
	}
}

// TestWatchFirstScreenshotCarriesMetadata. The first frame is a screenshot, which has no screencast
// metadata of its own, so it is read from the page's layout metrics; when that call fails the device
// size falls back to the frame's own.
func TestWatchFirstScreenshotCarriesMetadata(t *testing.T) {
	t.Run("from the layout metrics", func(t *testing.T) {
		fake, driver, id := watchSession(t)
		jpegAnswer(fake, tinyJPEG(t))
		fake.Handle("Page.getLayoutMetrics", func(cdptest.Call) (any, error) {
			return map[string]any{"cssVisualViewport": map[string]any{
				"clientWidth": 1024, "clientHeight": 768, "pageX": 5, "pageY": 7, "scale": 2,
			}}, nil
		})
		var seen frameLog
		w := startWatch(t, driver, id, seen.sink)
		defer w.stop(t)
		eventually(t, "the first frame", 3*time.Second, func() bool { return len(seen.all()) >= 1 })

		got := seen.all()[0]
		want := browser.FrameMeta{
			FrameWidth: 4, FrameHeight: 3,
			DeviceWidth: 1024, DeviceHeight: 768,
			OffsetTop: 0, PageScaleFactor: 2,
			ScrollOffsetX: 5, ScrollOffsetY: 7,
		}
		if got.Meta == nil {
			t.Fatal("the first screenshot carried no metadata")
		}
		if *got.Meta != want {
			t.Errorf("meta = %+v, want %+v", *got.Meta, want)
		}
	})

	t.Run("when the layout metrics fail", func(t *testing.T) {
		fake, driver, id := watchSession(t)
		jpegAnswer(fake, tinyJPEG(t))
		fake.Handle("Page.getLayoutMetrics", func(cdptest.Call) (any, error) {
			return nil, errors.New("no layout metrics")
		})
		var seen frameLog
		w := startWatch(t, driver, id, seen.sink)
		defer w.stop(t)
		eventually(t, "the first frame", 3*time.Second, func() bool { return len(seen.all()) >= 1 })

		got := seen.all()[0]
		want := browser.FrameMeta{
			FrameWidth: 4, FrameHeight: 3,
			DeviceWidth: 4, DeviceHeight: 3,
			PageScaleFactor: 1,
		}
		if got.Meta == nil {
			t.Fatal("the first screenshot carried no metadata")
		}
		if *got.Meta != want {
			t.Errorf("meta = %+v, want %+v", *got.Meta, want)
		}
	})
}

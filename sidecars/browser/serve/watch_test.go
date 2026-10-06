// §spec browser-ao-vivo

package serve

import (
	"bufio"
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// watchServer builds the mux the way Serve does for /watch: the driver is asked whether it is a
// Watcher, and the handler is given the answer (nil when it is not).
func watchServer(t *testing.T, driver browser.Driver, wrap func(http.HandlerFunc) http.HandlerFunc) *httptest.Server {
	t.Helper()
	watcher, _ := driver.(browser.Watcher)
	handler := authorized(token, watchHandler(watcher))
	if wrap != nil {
		handler = wrap(handler)
	}
	mux := http.NewServeMux()
	mux.HandleFunc("/watch", handler)
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

// scriptedWatcher is a Fake that can be watched: it pushes its frames, then ends the way its script says.
type scriptedWatcher struct {
	*browser.Fake
	// beforeEach runs before the frame at that index is sent, so a test can hold a frame back until
	// the client has proved it received the one before.
	beforeEach func(i int)
	frames     [][]byte
	// metas, when set, is the metadata sent with the frame at the same index.
	metas []*browser.FrameMeta
	end   error
}

func (s *scriptedWatcher) Watch(ctx context.Context, _ browser.SessionID, sink func(browser.Frame)) error {
	for i, jpeg := range s.frames {
		if s.beforeEach != nil {
			s.beforeEach(i)
		}
		frame := browser.Frame{JPEG: jpeg}
		if i < len(s.metas) {
			frame.Meta = s.metas[i]
		}
		sink(frame)
	}
	return s.end
}

func rawWatch(t *testing.T, server *httptest.Server, session string) *http.Response {
	t.Helper()
	return post(t, server, "/watch", SessionRequest{SessionID: session}, true)
}

func uint32BE(n int) []byte {
	var out [4]byte
	binary.BigEndian.PutUint32(out[:], uint32(n))
	return out[:]
}

// TestWatchRecordRoundTrip pins the wire format the shell parses: one kind byte, a four-byte
// big-endian length, then the body.
func TestWatchRecordRoundTrip(t *testing.T) {
	var wire bytes.Buffer
	if err := WriteRecord(&wire, RecordFrame, []byte("jpeg-bytes")); err != nil {
		t.Fatalf("write frame: %v", err)
	}
	if err := WriteRecord(&wire, RecordEnd, []byte(`{"reason":"closed"}`)); err != nil {
		t.Fatalf("write end: %v", err)
	}
	if err := WriteRecord(&wire, RecordFrame, nil); err != nil {
		t.Fatalf("write empty: %v", err)
	}

	if RecordFrame != 'F' || RecordEnd != 'E' {
		t.Fatalf("record kinds drifted: %q %q", RecordFrame, RecordEnd)
	}
	want := append([]byte{'F'}, uint32BE(len("jpeg-bytes"))...)
	if got := wire.Bytes()[:5]; !bytes.Equal(got, want) {
		t.Fatalf("header = % x, want % x", got, want)
	}

	kind, body, err := ReadRecord(&wire)
	if err != nil || kind != RecordFrame || string(body) != "jpeg-bytes" {
		t.Fatalf("first record = %q %q %v", kind, body, err)
	}
	kind, body, err = ReadRecord(&wire)
	if err != nil || kind != RecordEnd || string(body) != `{"reason":"closed"}` {
		t.Fatalf("second record = %q %q %v", kind, body, err)
	}
	kind, body, err = ReadRecord(&wire)
	if err != nil || kind != RecordFrame || len(body) != 0 {
		t.Fatalf("empty record = %q %q %v", kind, body, err)
	}
	if _, _, err := ReadRecord(&wire); !errors.Is(err, io.EOF) {
		t.Fatalf("after the last record: %v, want io.EOF", err)
	}
}

func TestWatchReadRecordRefusesWhatItCannotTrust(t *testing.T) {
	// A length over 16 MiB is refused before anything is allocated for it.
	huge := append([]byte{'F'}, uint32BE(16<<20+1)...)
	if _, _, err := ReadRecord(bytes.NewReader(huge)); err == nil {
		t.Fatal("a 16 MiB + 1 record was accepted")
	}
	// A body cut short is an error, not a short frame.
	cut := append(append([]byte{'F'}, uint32BE(10)...), []byte("abc")...)
	if _, _, err := ReadRecord(bytes.NewReader(cut)); err == nil {
		t.Fatal("a truncated record was accepted")
	}
}

// TestWatchStreamsFramesThenOneEndRecord. Frames must arrive while the watch is still running — the
// second frame is held back until the client has read the first, which a buffered-until-the-end
// response could never satisfy.
func TestWatchStreamsFramesThenOneEndRecord(t *testing.T) {
	firstRead := make(chan struct{})
	watcher := &scriptedWatcher{
		Fake:   &browser.Fake{FenceAttached: true},
		frames: [][]byte{[]byte("one"), []byte("two")},
		end:    browser.WatchEnded{Reason: browser.EndWheel},
		beforeEach: func(i int) {
			if i == 1 {
				select {
				case <-firstRead:
				case <-time.After(5 * time.Second):
				}
			}
		},
	}
	server := watchServer(t, watcher, nil)

	response := rawWatch(t, server, "s1")
	if response.StatusCode != http.StatusOK {
		t.Fatalf("got %d, want 200", response.StatusCode)
	}
	if got := response.Header.Get("Content-Type"); got != "application/octet-stream" {
		t.Fatalf("content type = %q", got)
	}

	reader := bufio.NewReader(response.Body)
	kind, body, err := ReadRecord(reader)
	if err != nil || kind != RecordFrame || string(body) != "one" {
		t.Fatalf("first record = %q %q %v", kind, body, err)
	}
	close(firstRead)
	kind, body, err = ReadRecord(reader)
	if err != nil || kind != RecordFrame || string(body) != "two" {
		t.Fatalf("second record = %q %q %v", kind, body, err)
	}
	kind, body, err = ReadRecord(reader)
	if err != nil || kind != RecordEnd {
		t.Fatalf("third record = %q %q %v, want the end record", kind, body, err)
	}
	var end struct {
		Reason string `json:"reason"`
	}
	if err := json.Unmarshal(body, &end); err != nil || end.Reason != "wheel" {
		t.Fatalf("end body = %q (%v), want reason wheel", body, err)
	}
	if _, _, err := ReadRecord(reader); !errors.Is(err, io.EOF) {
		t.Fatalf("after the end record: %v, want EOF — exactly one end record", err)
	}
}

// A watch that ends for no stated reason while the client is still there is "gone": the browser went
// away under the viewer.
func TestWatchAnUnexplainedEndIsGone(t *testing.T) {
	watcher := &scriptedWatcher{
		Fake:   &browser.Fake{FenceAttached: true},
		frames: [][]byte{[]byte("one")},
		end:    errors.New("cdp connection lost"),
	}
	server := watchServer(t, watcher, nil)

	reader := bufio.NewReader(rawWatch(t, server, "s1").Body)
	if kind, _, err := ReadRecord(reader); err != nil || kind != RecordFrame {
		t.Fatalf("first record = %q %v", kind, err)
	}
	kind, body, err := ReadRecord(reader)
	if err != nil || kind != RecordEnd || !strings.Contains(string(body), `"gone"`) {
		t.Fatalf("end record = %q %q %v, want reason gone", kind, body, err)
	}
}

// halfADriver is not a Watcher, and a driver that cannot stream says so: 501, not 500 — nothing failed.
func TestWatchWithoutAWatcherIs501(t *testing.T) {
	server := watchServer(t, halfADriver{}, nil)
	response := rawWatch(t, server, "s1")
	if response.StatusCode != http.StatusNotImplemented {
		t.Fatalf("got %d, want 501", response.StatusCode)
	}
}

// Before any frame the route has not committed to a stream, so a refusal is an ordinary status.
func TestWatchWhileAPersonDrivesIs409(t *testing.T) {
	watcher := &scriptedWatcher{
		Fake: &browser.Fake{FenceAttached: true},
		end:  fmt.Errorf("%w: s1", browser.ErrPersonIsDriving),
	}
	server := watchServer(t, watcher, nil)
	if got := rawWatch(t, server, "s1").StatusCode; got != http.StatusConflict {
		t.Fatalf("got %d, want 409", got)
	}

	missing := &scriptedWatcher{Fake: &browser.Fake{FenceAttached: true}, end: browser.ErrNoSuchSession}
	server = watchServer(t, missing, nil)
	if got := rawWatch(t, server, "s1").StatusCode; got != http.StatusNotFound {
		t.Fatalf("unknown session: got %d, want 404", got)
	}
}

func TestWatchRefusesAnEmptySessionAndAMissingToken(t *testing.T) {
	watcher := &scriptedWatcher{Fake: &browser.Fake{FenceAttached: true}}
	server := watchServer(t, watcher, nil)
	if got := rawWatch(t, server, "").StatusCode; got != http.StatusBadRequest {
		t.Fatalf("empty session: got %d, want 400", got)
	}
	if got := post(t, server, "/watch", SessionRequest{SessionID: "s1"}, false).StatusCode; got != http.StatusUnauthorized {
		t.Fatalf("no token: got %d, want 401", got)
	}
}

// stallingWatcher pushes 1 MiB frames from its own goroutine, so a sink that blocks on a client that
// never reads cannot stop Watch itself from being ended from outside — which is the contract on a
// Watcher ("a blocked sink must never stall the browser").
type stallingWatcher struct {
	*browser.Fake
	end      chan struct{}
	started  atomic.Int64
	returned atomic.Int64
}

func (s *stallingWatcher) Watch(ctx context.Context, _ browser.SessionID, sink func(browser.Frame)) error {
	done := make(chan struct{})
	defer close(done)
	frame := browser.Frame{JPEG: make([]byte, 1<<20)}
	go func() {
		for {
			select {
			case <-done:
				return
			default:
			}
			s.started.Add(1)
			sink(frame)
			s.returned.Add(1)
		}
	}()
	select {
	case <-s.end:
	case <-ctx.Done():
	}
	return browser.WatchEnded{Reason: browser.EndWheel}
}

// TestWatchStalledClientIsDroppedWithinTheWriteDeadline. The client never reads, so the socket fills
// and the sink blocks inside a write. Ending the watch must still free the handler: the one-second
// write deadline is what turns a blocked write into an error.
func TestWatchStalledClientIsDroppedWithinTheWriteDeadline(t *testing.T) {
	watcher := &stallingWatcher{Fake: &browser.Fake{FenceAttached: true}, end: make(chan struct{})}
	handlerDone := make(chan struct{})
	server := watchServer(t, watcher, func(next http.HandlerFunc) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			defer close(handlerDone)
			next(w, r)
		}
	})

	conn, err := net.Dial("tcp", strings.TrimPrefix(server.URL, "http://"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { conn.Close() })
	if tcp, ok := conn.(*net.TCPConn); ok {
		_ = tcp.SetReadBuffer(4096)
	}
	body := `{"session_id":"s1"}`
	request := fmt.Sprintf("POST /watch HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer %s\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n%s",
		token, len(body), body)
	if _, err := io.WriteString(conn, request); err != nil {
		t.Fatal(err)
	}

	// Stalled means a sink call has been in flight, unmoved, for a good while: the socket is full.
	deadline := time.Now().Add(20 * time.Second)
	var since time.Time
	var seen int64 = -1
	for {
		if time.Now().After(deadline) {
			t.Fatal("the socket never filled; the test could not stall the client")
		}
		started, returned := watcher.started.Load(), watcher.returned.Load()
		if started > returned && started == seen {
			if since.IsZero() {
				since = time.Now()
			}
			if time.Since(since) > 500*time.Millisecond {
				break
			}
		} else {
			since = time.Time{}
		}
		seen = started
		time.Sleep(50 * time.Millisecond)
	}

	ended := time.Now()
	close(watcher.end)
	select {
	case <-handlerDone:
	case <-time.After(2500 * time.Millisecond):
		t.Fatalf("the handler was still stuck %v after the watch ended", time.Since(ended))
	}
}

// sinkingWatcher keeps handing frames to the sink from Watch itself until its ctx is done, and says
// when that happened. It stands for a screencast that goes on encoding for as long as it is allowed.
type sinkingWatcher struct {
	*browser.Fake
	ctxEnded chan struct{}
}

func (s *sinkingWatcher) Watch(ctx context.Context, _ browser.SessionID, sink func(browser.Frame)) error {
	go func() {
		<-ctx.Done()
		close(s.ctxEnded)
	}()
	frame := browser.Frame{JPEG: make([]byte, 1<<20)}
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}
		sink(frame)
		time.Sleep(time.Millisecond)
	}
}

// brokenWrites is a ResponseWriter whose body writes fail while the connection underneath stays open
// and silent. net/http closes a connection whose write failed, which cancels the request context by
// itself, so the real socket cannot tell a handler that ends the watch from one that waits for that.
type brokenWrites struct{ http.ResponseWriter }

func (brokenWrites) Write([]byte) (int, error)     { return 0, errors.New("write failed") }
func (b brokenWrites) Unwrap() http.ResponseWriter { return b.ResponseWriter }

// TestWatchAFailedWriteEndsTheWatch. Once a record write has failed the client is gone for good, so
// the watch must stop at once: a screencast left running until the connection closes keeps encoding
// frames nobody will receive. The client here stays connected and silent, so only the failed write
// can end the watch.
func TestWatchAFailedWriteEndsTheWatch(t *testing.T) {
	watcher := &sinkingWatcher{Fake: &browser.Fake{FenceAttached: true}, ctxEnded: make(chan struct{})}
	server := watchServer(t, watcher, func(next http.HandlerFunc) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) { next(brokenWrites{w}, r) }
	})

	conn, err := net.Dial("tcp", strings.TrimPrefix(server.URL, "http://"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { conn.Close() })
	body := `{"session_id":"s1"}`
	request := fmt.Sprintf("POST /watch HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer %s\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n%s",
		token, len(body), body)
	sent := time.Now()
	if _, err := io.WriteString(conn, request); err != nil {
		t.Fatal(err)
	}

	select {
	case <-watcher.ctxEnded:
	case <-time.After(WatchWriteDeadline + 2*time.Second):
		t.Fatalf("the watch was still running %v after the request, though a record write had failed", time.Since(sent))
	}
}

// TestWatchWritesMBeforeTheFirstFrameAndWhenItChanges. The viewer needs the geometry before the
// picture it describes, and needs it again only when it changes: A, A, B reads back as M F F M F E.
func TestWatchWritesMBeforeTheFirstFrameAndWhenItChanges(t *testing.T) {
	a := &browser.FrameMeta{FrameWidth: 1280, FrameHeight: 720, DeviceWidth: 1280, DeviceHeight: 720, PageScaleFactor: 1}
	same := *a
	b := &browser.FrameMeta{FrameWidth: 1280, FrameHeight: 720, DeviceWidth: 1280, DeviceHeight: 720,
		OffsetTop: 12, PageScaleFactor: 2, ScrollOffsetX: 3, ScrollOffsetY: 40}
	watcher := &scriptedWatcher{
		Fake:   &browser.Fake{FenceAttached: true},
		frames: [][]byte{[]byte("one"), []byte("two"), []byte("three")},
		metas:  []*browser.FrameMeta{a, &same, b},
		end:    browser.WatchEnded{Reason: browser.EndWheel},
	}
	server := watchServer(t, watcher, nil)

	reader := bufio.NewReader(rawWatch(t, server, "s1").Body)
	var kinds []byte
	var metaBodies [][]byte
	var frameBodies []string
	for {
		kind, body, err := ReadRecord(reader)
		if err != nil {
			if !errors.Is(err, io.EOF) {
				t.Fatalf("read: %v", err)
			}
			break
		}
		kinds = append(kinds, kind)
		switch kind {
		case RecordMeta:
			metaBodies = append(metaBodies, body)
		case RecordFrame:
			frameBodies = append(frameBodies, string(body))
		}
	}
	if string(kinds) != "MFFMFE" {
		t.Fatalf("record kinds = %q, want MFFMFE", kinds)
	}
	if got := strings.Join(frameBodies, ","); got != "one,two,three" {
		t.Errorf("frames = %q, want them in order and untouched", got)
	}
	if len(metaBodies) != 2 {
		t.Fatalf("got %d M records, want 2", len(metaBodies))
	}
	var first, second browser.FrameMeta
	if err := json.Unmarshal(metaBodies[0], &first); err != nil || first != *a {
		t.Errorf("first M = %s (%v), want %+v", metaBodies[0], err, *a)
	}
	if err := json.Unmarshal(metaBodies[1], &second); err != nil || second != *b {
		t.Errorf("second M = %s (%v), want %+v", metaBodies[1], err, *b)
	}
	// The wire names are the contract the shell reads.
	var raw map[string]any
	if err := json.Unmarshal(metaBodies[1], &raw); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"frameWidth", "frameHeight", "deviceWidth", "deviceHeight",
		"offsetTop", "pageScaleFactor", "scrollOffsetX", "scrollOffsetY"} {
		if _, ok := raw[name]; !ok {
			t.Errorf("the M record has no %q field: %s", name, metaBodies[1])
		}
	}
}

// TestWatchRecordKindsAreFixed. The kind bytes are the wire format; adding M and P must not move F
// and E.
func TestWatchRecordKindsAreFixed(t *testing.T) {
	if RecordFrame != 'F' || RecordEnd != 'E' || RecordMeta != 'M' || RecordPrompt != 'P' {
		t.Fatalf("record kinds drifted: F=%q E=%q M=%q P=%q", RecordFrame, RecordEnd, RecordMeta, RecordPrompt)
	}
}

// promptingWatcher sinks the prompts and frames of its script in order, then ends the way it says.
type promptingWatcher struct {
	*browser.Fake
	script []browser.Frame
	end    error
}

func (p *promptingWatcher) Watch(_ context.Context, _ browser.SessionID, sink func(browser.Frame)) error {
	for _, frame := range p.script {
		sink(frame)
	}
	return p.end
}

// TestWatchWritesAPromptRecord. A prompt is one P record carrying the prompt as JSON, and nothing
// else: no F with an empty picture and no M for geometry it does not have. A resolved prompt is the
// same record with `resolved` set, and the stream goes on around both.
func TestWatchWritesAPromptRecord(t *testing.T) {
	multiple := false
	ask := &browser.Prompt{ID: "p1", Kind: "dialog", DialogType: "confirm", Message: "Delete everything?"}
	choose := &browser.Prompt{ID: "p2", Kind: "select", Multiple: &multiple,
		Options: []browser.PromptOption{{Value: "a", Label: "Alpha", Selected: true}}}
	watcher := &promptingWatcher{
		Fake: &browser.Fake{FenceAttached: true},
		script: []browser.Frame{
			{JPEG: []byte("one")},
			{Prompt: ask},
			{Prompt: choose},
			{Prompt: &browser.Prompt{ID: "p1", Kind: "dialog", Resolved: true}},
			{JPEG: []byte("two")},
		},
		end: browser.WatchEnded{Reason: browser.EndGone},
	}
	server := watchServer(t, watcher, nil)

	reader := bufio.NewReader(rawWatch(t, server, "s1").Body)
	var kinds []byte
	var prompts [][]byte
	for {
		kind, body, err := ReadRecord(reader)
		if err != nil {
			if !errors.Is(err, io.EOF) {
				t.Fatalf("read: %v", err)
			}
			break
		}
		kinds = append(kinds, kind)
		if kind == RecordPrompt {
			prompts = append(prompts, body)
		}
	}
	if string(kinds) != "FPPPFE" {
		t.Fatalf("record kinds = %q, want FPPPFE", kinds)
	}
	if len(prompts) != 3 {
		t.Fatalf("got %d P records, want 3", len(prompts))
	}

	var first map[string]any
	if err := json.Unmarshal(prompts[0], &first); err != nil {
		t.Fatalf("the first P is not JSON: %v", err)
	}
	if first["id"] != "p1" || first["kind"] != "dialog" || first["dialogType"] != "confirm" || first["message"] != "Delete everything?" {
		t.Errorf("first P = %s", prompts[0])
	}
	if _, present := first["resolved"]; present {
		t.Errorf("an open prompt carries `resolved`: %s", prompts[0])
	}

	var second map[string]any
	if err := json.Unmarshal(prompts[1], &second); err != nil {
		t.Fatalf("the second P is not JSON: %v", err)
	}
	if value, present := second["multiple"]; !present || value != false {
		t.Errorf("a select prompt must say multiple=false out loud: %s", prompts[1])
	}
	if options, ok := second["options"].([]any); !ok || len(options) != 1 {
		t.Errorf("the select's options were lost: %s", prompts[1])
	}

	var closed map[string]any
	if err := json.Unmarshal(prompts[2], &closed); err != nil {
		t.Fatalf("the third P is not JSON: %v", err)
	}
	if closed["id"] != "p1" || closed["kind"] != "dialog" || closed["resolved"] != true {
		t.Errorf("resolved P = %s, want {id, kind, resolved:true}", prompts[2])
	}
	if _, present := closed["message"]; present {
		t.Errorf("a resolved record repeats the question: %s", prompts[2])
	}
}

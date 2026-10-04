//go:build browsergate

// §spec browser-ao-vivo

package gate_test

import (
	"bytes"
	"context"
	"fmt"
	"image/jpeg"
	"math"
	"net/http"
	"net/http/httptest"
	"strconv"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/fence"
)

// TestWatchFrameMatchesTheViewport.
//
// What the unit tests cannot say: that the picture a viewer is handed has the SHAPE of the page the
// agent sees. The screencast is capped at 1280 on its long side, and the first frame is a
// screenshot; both must come out as the viewport in device pixels, scaled down proportionally when
// it is wider than the cap, or the pane would show a squashed or cropped page and every coordinate
// a person pointed at would be off.
//
// Against real Chromium, because the viewport and the device pixel ratio are facts about the
// browser. The page reports its own numbers to the server, so nothing here assumes a window size.
func TestWatchFrameMatchesTheViewport(t *testing.T) {
	dims := make(chan [3]float64, 4)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/dims" {
			var got [3]float64
			for i, key := range []string{"w", "h", "dpr"} {
				got[i], _ = strconv.ParseFloat(r.URL.Query().Get(key), 64)
			}
			select {
			case dims <- got:
			default:
			}
			return
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>watch</title><h1>watch</h1>
			<script>fetch('/dims?w='+innerWidth+'&h='+innerHeight+'&dpr='+devicePixelRatio)</script>`)
	}))
	t.Cleanup(server.Close)

	driver, _ := fenced(t, fence.Policy{
		Profile:  fence.Project,
		Origins:  []string{"https://nucleos.invalid"},
		Loopback: []string{server.URL},
	})
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: server.URL + "/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}

	var viewport [3]float64
	select {
	case viewport = <-dims:
	case <-time.After(15 * time.Second):
		t.Fatal("the page never reported its viewport")
	}
	innerWidth, innerHeight, dpr := viewport[0], viewport[1], viewport[2]
	if innerWidth <= 0 || innerHeight <= 0 || dpr <= 0 {
		t.Fatalf("nonsense viewport reported: %v", viewport)
	}

	frames := make(chan browser.Frame, 1)
	watchCtx, stop := context.WithCancel(ctx)
	defer stop()
	go func() {
		_ = driver.Watch(watchCtx, session.ID, func(frame browser.Frame) {
			select {
			case frames <- frame:
			default:
			}
		})
	}()

	var frame browser.Frame
	select {
	case frame = <-frames:
	case <-time.After(20 * time.Second):
		t.Fatal("no frame arrived")
	}
	stop()

	config, err := jpeg.DecodeConfig(bytes.NewReader(frame.JPEG))
	if err != nil {
		t.Fatalf("the frame is not a JPEG: %v", err)
	}

	wantWidth := math.Min(1280, innerWidth*dpr)
	wantHeight := wantWidth * innerHeight / innerWidth
	if math.Abs(float64(config.Width)-wantWidth) > 2 {
		t.Errorf("frame width = %d, want %.0f (min(1280, %v x %v))", config.Width, wantWidth, innerWidth, dpr)
	}
	if math.Abs(float64(config.Height)-wantHeight) > 2 {
		t.Errorf("frame height = %d, want %.0f, the viewport's proportion", config.Height, wantHeight)
	}
}

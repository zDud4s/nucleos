// §spec browser-ao-vivo

package chrome

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"image/jpeg"
	"sync"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// ackGap is the least time between two acknowledgements. The ack is the only brake Chrome has on a
// screencast, so this is what caps the frame rate at about ten a second.
const ackGap = 100 * time.Millisecond

type incomingFrame struct {
	data  string
	ackID int
	meta  screencastMeta
}

// screencastMeta is the metadata Chrome sends beside each screencast frame.
type screencastMeta struct {
	OffsetTop       float64 `json:"offsetTop"`
	PageScaleFactor float64 `json:"pageScaleFactor"`
	DeviceWidth     float64 `json:"deviceWidth"`
	DeviceHeight    float64 `json:"deviceHeight"`
	ScrollOffsetX   float64 `json:"scrollOffsetX"`
	ScrollOffsetY   float64 `json:"scrollOffsetY"`
}

// frameDims reads a JPEG's size from its header; zero when it does not decode.
func frameDims(raw []byte) (int, int) {
	cfg, err := jpeg.DecodeConfig(bytes.NewReader(raw))
	if err != nil {
		return 0, 0
	}
	return cfg.Width, cfg.Height
}

// viewer holds the newest frame it has not been shown. Capacity one, replace-oldest: a slow viewer
// sees the latest picture, never a queue of old ones.
type viewer struct {
	slot chan browser.Frame
	// prompts carries the questions put to a person, in order. Unlike slot it is a queue: a prompt is
	// not superseded by the next one. It is never blocked on; a viewer that falls 32 behind misses one.
	prompts chan browser.Frame
}

// sendPrompt queues a prompt for the viewer without ever blocking the caller.
func (v *viewer) sendPrompt(prompt browser.Prompt) {
	select {
	case v.prompts <- browser.Frame{Prompt: &prompt}:
	default:
	}
}

func (v *viewer) put(frame browser.Frame) {
	select {
	case <-v.slot:
	default:
	}
	select {
	case v.slot <- frame:
	default:
	}
}

// screencast is the one shared stream of a session, however many people watch it.
type screencast struct {
	session  cdp.SessionID
	viewers  map[*viewer]struct{} // guarded by Driver.mu
	incoming chan incomingFrame
	done     chan struct{} // the session ended
	doneOnce sync.Once
	stop     chan struct{} // the last viewer left
}

func (c *screencast) end() { c.doneOnce.Do(func() { close(c.done) }) }

// Watch streams the session's page to sink until ctx ends or the session does.
//
// It runs in every mode, a person's turn included: the viewer is waiting to see the login, and the
// screencast only reads the page. It does not take the person gate for that reason, and for another —
// a stream lasts as long as the viewer does, and a swap would wait on it forever.
func (d *Driver) Watch(ctx context.Context, id browser.SessionID, sink func(browser.Frame)) error {
	entry, err := d.lookup(id)
	if err != nil {
		return err
	}

	d.castCtl.Lock()
	// While a person drives, the pending prompts are held still from here until this viewer is
	// registered, so it is shown every unresolved one exactly once: the ones raised before are
	// replayed below, and the ones raised after reach it as a viewer.
	state := d.person.Load()
	if state != nil {
		state.pmu.Lock()
	}
	d.mu.Lock()
	if _, ok := d.sessions[id]; !ok {
		d.mu.Unlock()
		if state != nil {
			state.pmu.Unlock()
		}
		d.castCtl.Unlock()
		return browser.ErrNoSuchSession
	}
	me := &viewer{slot: make(chan browser.Frame, 1), prompts: make(chan browser.Frame, 32)}
	cast, running := d.casts[entry.cdp]
	if !running {
		cast = &screencast{
			session:  entry.cdp,
			viewers:  map[*viewer]struct{}{},
			incoming: make(chan incomingFrame, 1),
			done:     make(chan struct{}),
			stop:     make(chan struct{}),
		}
		d.casts[entry.cdp] = cast
	}
	cast.viewers[me] = struct{}{}
	d.mu.Unlock()
	if state != nil {
		for _, pendingID := range state.order {
			if pending := state.pending[pendingID]; pending != nil && pending.on == entry.cdp {
				me.sendPrompt(pending.prompt)
			}
		}
		state.pmu.Unlock()
	}

	if !running {
		_, startErr := d.conn.Call(ctx, entry.cdp, "Page.startScreencast", map[string]any{
			"format": "jpeg", "quality": 60, "maxWidth": 1280, "maxHeight": 1280,
		})
		if startErr != nil {
			d.mu.Lock()
			delete(cast.viewers, me)
			if d.casts[entry.cdp] == cast {
				delete(d.casts, entry.cdp)
			}
			d.mu.Unlock()
			close(cast.stop)
			d.castCtl.Unlock()
			return startErr
		}
		go d.pumpScreencast(cast)
	}
	d.castCtl.Unlock()
	defer d.leaveScreencast(entry, cast, me)

	// A screencast sends a frame when the page paints and a static page does not, so every viewer is
	// handed a screenshot at once.
	if shot, shotErr := d.conn.Call(ctx, entry.cdp, "Page.captureScreenshot", map[string]any{
		"format": "jpeg", "quality": 60,
	}); shotErr == nil {
		var payload struct {
			Data string `json:"data"`
		}
		if json.Unmarshal(shot, &payload) == nil {
			if raw, decErr := base64.StdEncoding.DecodeString(payload.Data); decErr == nil {
				me.put(browser.Frame{JPEG: raw, Meta: d.screenshotMeta(ctx, entry.cdp, raw)})
			}
		}
	}

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-cast.done:
			return browser.ErrNoSuchSession
		case frame := <-me.prompts:
			if err := ctx.Err(); err != nil {
				return err
			}
			sink(frame)
		case frame := <-me.slot:
			// select picks at random between ready cases, so a cancelled ctx or a dead target must be
			// checked again here: no frame goes to the sink once the watch is over.
			if err := ctx.Err(); err != nil {
				return err
			}
			select {
			case <-cast.done:
				return browser.ErrNoSuchSession
			default:
			}
			sink(frame)
		}
	}
}

// leaveScreencast unregisters one viewer, and stops the screencast when it was the last.
func (d *Driver) leaveScreencast(entry *session, cast *screencast, me *viewer) {
	d.castCtl.Lock()
	defer d.castCtl.Unlock()
	d.mu.Lock()
	delete(cast.viewers, me)
	last := len(cast.viewers) == 0
	if last && d.casts[entry.cdp] == cast {
		delete(d.casts, entry.cdp)
	}
	d.mu.Unlock()
	if !last {
		return
	}
	close(cast.stop)
	select {
	case <-cast.done:
		return // the target is gone; there is nothing to stop
	default:
	}
	// A fresh context: the viewer's own is probably what ended this.
	stopCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_, _ = d.conn.Call(stopCtx, entry.cdp, "Page.stopScreencast", map[string]any{})
}

// screenshotMeta is the geometry of the first screenshot, which has no screencast metadata of its
// own: it comes from the page's layout metrics, and falls back to the frame's own size.
func (d *Driver) screenshotMeta(ctx context.Context, session cdp.SessionID, raw []byte) *browser.FrameMeta {
	width, height := frameDims(raw)
	meta := &browser.FrameMeta{
		FrameWidth: width, FrameHeight: height,
		DeviceWidth: float64(width), DeviceHeight: float64(height),
		PageScaleFactor: 1,
	}
	reply, err := d.conn.Call(ctx, session, "Page.getLayoutMetrics", map[string]any{})
	if err != nil {
		return meta
	}
	var metrics struct {
		Viewport struct {
			ClientWidth  float64 `json:"clientWidth"`
			ClientHeight float64 `json:"clientHeight"`
			PageX        float64 `json:"pageX"`
			PageY        float64 `json:"pageY"`
			Scale        float64 `json:"scale"`
		} `json:"cssVisualViewport"`
	}
	if json.Unmarshal(reply, &metrics) != nil {
		return meta
	}
	v := metrics.Viewport
	if v.ClientWidth > 0 && v.ClientHeight > 0 {
		meta.DeviceWidth, meta.DeviceHeight = v.ClientWidth, v.ClientHeight
	}
	meta.ScrollOffsetX, meta.ScrollOffsetY = v.PageX, v.PageY
	if v.Scale > 0 {
		meta.PageScaleFactor = v.Scale
	}
	return meta
}

// onScreencastFrame runs on the connection's one dispatch goroutine, which the fence also answers
// on, so it only hands the frame over and returns. It must never call out: a call from here waits
// on a reply that only this goroutine can deliver.
func (d *Driver) onScreencastFrame(event cdp.Event) {
	if event.Method != "Page.screencastFrame" {
		return
	}
	var params struct {
		Data      string         `json:"data"`
		SessionID int            `json:"sessionId"`
		Metadata  screencastMeta `json:"metadata"`
	}
	if json.Unmarshal(event.Params, &params) != nil {
		return
	}
	d.mu.Lock()
	cast := d.casts[event.Session]
	d.mu.Unlock()
	if cast == nil {
		return
	}
	frame := incomingFrame{data: params.Data, ackID: params.SessionID, meta: params.Metadata}
	select {
	case <-cast.incoming:
	default:
	}
	select {
	case cast.incoming <- frame:
	default:
	}
}

// pumpScreencast decodes frames, gives each to every viewer, and acknowledges them no faster than
// ackGap apart.
func (d *Driver) pumpScreencast(cast *screencast) {
	var lastAck time.Time
	for {
		var frame incomingFrame
		select {
		case <-cast.stop:
			return
		case <-cast.done:
			return
		case frame = <-cast.incoming:
		}
		if raw, err := base64.StdEncoding.DecodeString(frame.data); err == nil {
			width, height := frameDims(raw)
			shown := browser.Frame{JPEG: raw, Meta: &browser.FrameMeta{
				FrameWidth: width, FrameHeight: height,
				DeviceWidth: frame.meta.DeviceWidth, DeviceHeight: frame.meta.DeviceHeight,
				OffsetTop: frame.meta.OffsetTop, PageScaleFactor: frame.meta.PageScaleFactor,
				ScrollOffsetX: frame.meta.ScrollOffsetX, ScrollOffsetY: frame.meta.ScrollOffsetY,
			}}
			d.mu.Lock()
			for v := range cast.viewers {
				v.put(shown)
			}
			d.mu.Unlock()
		}
		if wait := ackGap - time.Since(lastAck); wait > 0 {
			timer := time.NewTimer(wait)
			select {
			case <-timer.C:
			case <-cast.stop:
				timer.Stop()
				return
			case <-cast.done:
				timer.Stop()
				return
			}
		}
		ackCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		_, _ = d.conn.Call(ackCtx, cast.session, "Page.screencastFrameAck", map[string]any{"sessionId": frame.ackID})
		cancel()
		lastAck = time.Now()
	}
}

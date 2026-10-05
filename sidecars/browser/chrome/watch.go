// §spec browser-ao-vivo

package chrome

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
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
}

// viewer holds the newest frame it has not been shown. Capacity one, replace-oldest: a slow viewer
// sees the latest picture, never a queue of old ones.
type viewer struct{ slot chan browser.Frame }

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
func (d *Driver) Watch(ctx context.Context, id browser.SessionID, sink func(browser.Frame)) error {
	entry, err := d.lookup(id)
	if err != nil {
		return err
	}

	d.castCtl.Lock()
	d.mu.Lock()
	if _, ok := d.sessions[id]; !ok {
		d.mu.Unlock()
		d.castCtl.Unlock()
		return browser.ErrNoSuchSession
	}
	if entry.mode != browser.ModeAgent {
		d.mu.Unlock()
		d.castCtl.Unlock()
		return fmt.Errorf("%w: the screencast belongs to the agent's browser", browser.ErrPersonIsDriving)
	}
	me := &viewer{slot: make(chan browser.Frame, 1)}
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
				me.put(browser.Frame{JPEG: raw})
			}
		}
	}

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-cast.done:
			return browser.ErrNoSuchSession
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

// onScreencastFrame runs on the connection's one dispatch goroutine, which the fence also answers
// on, so it only hands the frame over and returns. It must never call out: a call from here waits
// on a reply that only this goroutine can deliver.
func (d *Driver) onScreencastFrame(event cdp.Event) {
	if event.Method != "Page.screencastFrame" {
		return
	}
	var params struct {
		Data      string `json:"data"`
		SessionID int    `json:"sessionId"`
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
	frame := incomingFrame{data: params.Data, ackID: params.SessionID}
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
			shown := browser.Frame{JPEG: raw}
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

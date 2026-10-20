package fakecore

import (
	"context"
	"fmt"
	"io"
	"net/http"

	"nucleoschannelkit/contract"
)

// storedMedia is one uploaded (or seeded) media.
type storedMedia struct {
	mime string
	data []byte
}

// readBody reads at most limit bytes and fails when the body is larger.
func readBody(r *http.Request, limit int64) ([]byte, error) {
	b, err := io.ReadAll(io.LimitReader(r.Body, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(b)) > limit {
		return nil, fmt.Errorf("body over %d bytes", limit)
	}
	return b, nil
}

func (c *Core) mountEvents() {
	c.mux.HandleFunc("POST /channels/{channel}/events", c.authed(RouteEvents, c.handleEvent))
}

func (c *Core) mountMedia() {
	c.mux.HandleFunc("POST /channels/{channel}/media", c.authed(RouteMedia, c.handleMediaUpload))
	c.mux.HandleFunc("GET /channels/{channel}/media/{id}", c.authed(RouteMedia, c.handleMediaDownload))
}

// handleEvent validates the body strictly and dedupes by (channel, event_id).
// The token is bound to one channel, so the event id alone is the key here.
func (c *Core) handleEvent(w http.ResponseWriter, r *http.Request) {
	body, err := readBody(r, 1<<20)
	if err != nil {
		w.WriteHeader(http.StatusBadRequest)
		return
	}
	ev, err := contract.DecodeStrict[contract.Event](body)
	if err != nil {
		c.mu.Lock()
		c.invalid = append(c.invalid, err.Error())
		c.mu.Unlock()
		writeContractError(w, err)
		return
	}
	key := c.Channel() + "\x00" + *ev.EventID
	c.mu.Lock()
	if c.seen[key] {
		c.mu.Unlock()
		writeJSON(w, http.StatusOK, map[string]bool{"duplicate": true})
		return
	}
	c.seen[key] = true
	c.events = append(c.events, ev)
	c.notifyLocked()
	c.mu.Unlock()
	writeJSON(w, http.StatusAccepted, map[string]bool{"accepted": true})
}

// Events returns a copy of the accepted events, in arrival order.
func (c *Core) Events() []contract.Event {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]contract.Event(nil), c.events...)
}

// InvalidEvents lists the contract violations of rejected event bodies.
func (c *Core) InvalidEvents() []string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]string(nil), c.invalid...)
}

// WaitEvents blocks until at least n events were accepted, or ctx ends. It waits
// on a notification from the server, never on time.
func (c *Core) WaitEvents(ctx context.Context, n int) ([]contract.Event, error) {
	for {
		c.mu.Lock()
		if len(c.events) >= n {
			out := append([]contract.Event(nil), c.events...)
			c.mu.Unlock()
			return out, nil
		}
		ch := c.changed
		c.mu.Unlock()
		select {
		case <-ch:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
}

// SetMediaLimit sets the largest upload accepted (default 20971520).
func (c *Core) SetMediaLimit(n int64) {
	c.mu.Lock()
	c.mediaLimit = n
	c.mu.Unlock()
}

// AddMedia seeds a media (an inbound attachment) and returns its id.
func (c *Core) AddMedia(mimeType string, data []byte) string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.addMediaLocked(mimeType, data)
}

func (c *Core) addMediaLocked(mimeType string, data []byte) string {
	c.mediaSeq++
	id := fmt.Sprintf("m_%d", c.mediaSeq)
	c.media[id] = storedMedia{mime: mimeType, data: append([]byte(nil), data...)}
	return id
}

// Media returns the bytes and mime of a stored media.
func (c *Core) Media(id string) (data []byte, mimeType string, ok bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	m, ok := c.media[id]
	return m.data, m.mime, ok
}

func (c *Core) handleMediaUpload(w http.ResponseWriter, r *http.Request) {
	c.mu.Lock()
	limit := c.mediaLimit
	c.mu.Unlock()
	data, err := readBody(r, limit)
	if err != nil {
		c.writeFixture(w, http.StatusRequestEntityTooLarge, "errors/media-too-large.json")
		return
	}
	c.mu.Lock()
	id := c.addMediaLocked(r.Header.Get("Content-Type"), data)
	c.notifyLocked()
	c.mu.Unlock()
	writeJSON(w, http.StatusOK, map[string]string{"media_id": id})
}

func (c *Core) handleMediaDownload(w http.ResponseWriter, r *http.Request) {
	data, mimeType, ok := c.Media(r.PathValue("id"))
	if !ok {
		writeJSON(w, http.StatusNotFound, map[string]string{"error": "not_found"})
		return
	}
	if mimeType != "" {
		w.Header().Set("Content-Type", mimeType)
	}
	_, _ = w.Write(data)
}

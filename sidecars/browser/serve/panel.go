// §spec browser-com-painel

package serve

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"sync"
	"time"

	"nucleosbrowser/browser"
)

// panelRequest names the session a panel call is about; push also carries the message.
type panelRequest struct {
	Session string          `json:"session"`
	Message json.RawMessage `json:"message"`
}

// panelFailure answers a panel call's failure as a JSON error.
func panelFailure(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, browser.ErrNoSuchSession):
		writeJSONError(w, http.StatusNotFound, "no_session")
	case errors.Is(err, browser.ErrUnsupported):
		writeJSONError(w, http.StatusNotImplemented, "unsupported")
	default:
		writeJSONError(w, http.StatusInternalServerError, err.Error())
	}
}

// panelRoutes registers /panel/push and /panel/events. A driver with no panel answers 501.
func panelRoutes(mux *http.ServeMux, token string, driver browser.Driver) {
	panel, _ := driver.(browser.Panel)
	mux.HandleFunc("/panel/push", authorized(token, func(w http.ResponseWriter, r *http.Request) {
		var request panelRequest
		if !decodeJSON(w, r, &request, MaxBody) {
			return
		}
		if panel == nil {
			writeJSONError(w, http.StatusNotImplemented, "unsupported")
			return
		}
		if err := panel.PanelPush(r.Context(), browser.SessionID(request.Session), request.Message); err != nil {
			panelFailure(w, err)
			return
		}
		writeJSON(w, struct{}{})
	}))
	mux.HandleFunc("/panel/events", authorized(token, panelEventsHandler(panel)))
}

// panelEventsHandler streams the panel's events as N records and ends with one E record.
//
// Like /watch, the status line is committed on the first record, so a refusal before it is an ordinary
// status; every touch of w is under mu because the driver may call the sink from its own goroutine.
func panelEventsHandler(panel browser.Panel) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request panelRequest
		if !decodeJSON(w, r, &request, MaxBody) {
			return
		}
		if panel == nil {
			writeJSONError(w, http.StatusNotImplemented, "unsupported")
			return
		}

		ctx, cancel := context.WithCancel(r.Context())
		defer cancel()

		rc := http.NewResponseController(w)
		var (
			mu       sync.Mutex
			started  bool
			failed   bool
			finished bool
		)
		write := func(kind byte, body []byte) {
			if failed || finished {
				return
			}
			if !started {
				w.Header().Set("Content-Type", "application/octet-stream")
				w.WriteHeader(http.StatusOK)
				started = true
			}
			_ = rc.SetWriteDeadline(time.Now().Add(WatchWriteDeadline))
			if err := WriteRecord(w, kind, body); err != nil {
				failed = true
				cancel()
				return
			}
			if err := rc.Flush(); err != nil {
				failed = true
				cancel()
			}
		}
		sink := func(event json.RawMessage) {
			mu.Lock()
			defer mu.Unlock()
			write(RecordPanel, event)
		}

		err := panel.PanelEvents(ctx, browser.SessionID(request.Session), sink)

		mu.Lock()
		defer mu.Unlock()
		defer func() { finished = true }()
		var closed browser.PanelClosed
		isEnd := errors.As(err, &closed)
		if !started && err != nil && !isEnd {
			panelFailure(w, err)
			return
		}
		if r.Context().Err() != nil {
			return
		}
		reason := browser.PanelEnd("gone")
		if isEnd {
			reason = closed.Reason
		}
		body, _ := json.Marshal(struct {
			Reason browser.PanelEnd `json:"reason"`
		}{reason})
		write(RecordEnd, body)
	}
}

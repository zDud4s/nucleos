// §spec pilar-de-browser

package serve

import (
	"errors"
	"net/http"
	"strings"

	"nucleosbrowser/browser"
)

// forgetEmbeddedRequest names the embedded project to drop.
type forgetEmbeddedRequest struct {
	Project string `json:"project"`
}

// embeddedRoutes registers /embedded/prepare, /embedded/attach, /embedded/forget, /embedded/detach and
// /embedded/reattach, the daemon's way of fencing a tab of the person's own WebView2. Detach and
// reattach take {"session_id"}; detach answers 204 and reattach answers 200 {"chain":[...]}, the
// main-frame documents requested while detached. An unknown session is 404. A driver that cannot
// answers 501.
func embeddedRoutes(mux *http.ServeMux, token string, driver browser.Driver) {
	embedded, _ := driver.(browser.Embedded)

	mux.HandleFunc("/embedded/prepare", authorized(token, func(w http.ResponseWriter, r *http.Request) {
		var request browser.PrepareRequest
		if !decode(w, r, &request) {
			return
		}
		if embedded == nil {
			http.Error(w, "this driver cannot fence an embedded browser", http.StatusNotImplemented)
			return
		}
		if strings.TrimSpace(request.Project) == "" {
			http.Error(w, "project is required", http.StatusBadRequest)
			return
		}
		if err := request.Placement.Profile.Validate(); err != nil {
			http.Error(w, "placement: "+err.Error(), http.StatusBadRequest)
			return
		}
		reply, err := embedded.PrepareEmbedded(r.Context(), request)
		if err != nil {
			writeEmbeddedError(w, "embedded/prepare", err)
			return
		}
		writeJSON(w, reply)
	}))

	mux.HandleFunc("/embedded/attach", authorized(token, func(w http.ResponseWriter, r *http.Request) {
		var request browser.AttachRequest
		if !decode(w, r, &request) {
			return
		}
		if embedded == nil {
			http.Error(w, "this driver cannot fence an embedded browser", http.StatusNotImplemented)
			return
		}
		for field, value := range map[string]string{
			"project":  request.Project,
			"cdp_http": request.CDPHTTP,
			"nonce":    request.Nonce,
			"url":      request.URL,
		} {
			if strings.TrimSpace(value) == "" {
				http.Error(w, field+" is required", http.StatusBadRequest)
				return
			}
		}
		if err := request.Placement.Profile.Validate(); err != nil {
			http.Error(w, "placement: "+err.Error(), http.StatusBadRequest)
			return
		}
		session, err := embedded.AttachEmbedded(r.Context(), request)
		if err != nil {
			writeEmbeddedError(w, "embedded/attach", err)
			return
		}
		writeJSON(w, session)
	}))

	mux.HandleFunc("/embedded/forget", authorized(token, func(w http.ResponseWriter, r *http.Request) {
		var request forgetEmbeddedRequest
		if !decode(w, r, &request) {
			return
		}
		if embedded == nil {
			http.Error(w, "this driver cannot fence an embedded browser", http.StatusNotImplemented)
			return
		}
		if strings.TrimSpace(request.Project) == "" {
			http.Error(w, "project is required", http.StatusBadRequest)
			return
		}
		if err := embedded.ForgetEmbedded(r.Context(), request.Project); err != nil {
			writeEmbeddedError(w, "embedded/forget", err)
			return
		}
		w.WriteHeader(http.StatusNoContent)
	}))

	// sessionRoute serves a verb on one embedded session: 204 when call answers nil, else the JSON of
	// what it answers.
	sessionRoute := func(path string, call func(browser.Embedded, *http.Request, browser.SessionID) (any, error)) {
		mux.HandleFunc(path, authorized(token, func(w http.ResponseWriter, r *http.Request) {
			var request browser.EmbeddedSessionRequest
			if !decode(w, r, &request) {
				return
			}
			if embedded == nil {
				http.Error(w, "this driver cannot fence an embedded browser", http.StatusNotImplemented)
				return
			}
			if strings.TrimSpace(string(request.SessionID)) == "" {
				http.Error(w, "session_id is required", http.StatusBadRequest)
				return
			}
			reply, err := call(embedded, r, request.SessionID)
			if err != nil {
				writeEmbeddedError(w, strings.TrimPrefix(path, "/"), err)
				return
			}
			if reply == nil {
				w.WriteHeader(http.StatusNoContent)
				return
			}
			writeJSON(w, reply)
		}))
	}
	sessionRoute("/embedded/detach", func(e browser.Embedded, r *http.Request, id browser.SessionID) (any, error) {
		return nil, e.DetachEmbedded(r.Context(), id)
	})
	sessionRoute("/embedded/reattach", func(e browser.Embedded, r *http.Request, id browser.SessionID) (any, error) {
		returned, err := e.ReattachEmbedded(r.Context(), id)
		if err != nil {
			return nil, err
		}
		if returned.Chain == nil {
			returned.Chain = []string{}
		}
		return returned, nil
	})
}

// writeEmbeddedError maps the embedded sentinels, then falls back to the common mapping. A fence
// refusal of a navigation is not an error here: it travels inside the 200 Session, as for /open.
func writeEmbeddedError(w http.ResponseWriter, verb string, err error) {
	switch {
	case errors.Is(err, browser.ErrBadCDPAddress), errors.Is(err, browser.ErrBadAttach):
		http.Error(w, err.Error(), http.StatusBadRequest)
	case errors.Is(err, browser.ErrNoSuchTarget):
		http.Error(w, "no such target", http.StatusNotFound)
	case errors.Is(err, browser.ErrNotPrepared), errors.Is(err, browser.ErrPlacementMismatch),
		errors.Is(err, browser.ErrEmbeddedPolicyChanged):
		// 409: the request is well formed, and the project's state is what refuses it.
		http.Error(w, err.Error(), http.StatusConflict)
	default:
		writeDriverError(w, verb, err)
	}
}

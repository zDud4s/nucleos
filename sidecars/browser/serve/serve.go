// Package serve is this sidecar's only inbound surface.
//
// It binds to loopback, requires the daemon's token, and writes nothing anywhere. The núcleo asks
// for a session, a snapshot or an action; this process answers. No cursor, no cache, no database —
// all of that lives in SQLite, which this process has never been able to open (spec §4, §5).
//
// # The one thing this package must not turn into an error
//
// A fence refusal (spec §6.2) comes back as HTTP 200 carrying outcome="refused". It is an answer.
// Mapping it to 4xx would make it indistinguishable from a malformed request, and mapping it to 5xx
// would make it indistinguishable from a crashed browser — and in both cases the caller's natural
// reaction is to retry the one action the fence just refused.
package serve

import (
	"crypto/subtle"
	"encoding/json"
	"errors"
	"log"
	"net/http"
	"strings"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/config"
	"nucleosbrowser/profile"
)

// HeaderTimeout bounds how long a client may take to send its headers. Small, because the only
// legitimate client is on the same machine.
const HeaderTimeout = 10 * time.Second

// MaxBody caps a request body. Every request here is a handful of short fields.
const MaxBody = 64 << 10

// Serve blocks, answering the núcleo until the process ends.
func Serve(cfg config.Config, driver browser.Driver) error {
	mux := http.NewServeMux()
	mux.HandleFunc("/open", authorized(cfg.DaemonToken, openHandler(driver)))
	mux.HandleFunc("/snapshot", authorized(cfg.DaemonToken, snapshotHandler(driver)))
	mux.HandleFunc("/act", authorized(cfg.DaemonToken, actHandler(driver)))
	mux.HandleFunc("/screenshot", authorized(cfg.DaemonToken, screenshotHandler(driver)))
	mux.HandleFunc("/look", authorized(cfg.DaemonToken, lookHandler(driver)))
	mux.HandleFunc("/handoff", authorized(cfg.DaemonToken, handoffHandler(driver)))
	mux.HandleFunc("/close", authorized(cfg.DaemonToken, closeHandler(driver)))

	// The wheel (spec §4.4). Not agent verbs: no tool reaches these, and the only caller is the
	// núcleo acting on a person's answer to a proposal. A driver that cannot swap processes does not
	// implement Wheelhouse, and these answer 501 for it rather than pretending to hand over.
	wheelhouse, _ := driver.(browser.Wheelhouse)
	mux.HandleFunc("/wheel/take", authorized(cfg.DaemonToken, takeWheelHandler(wheelhouse)))
	mux.HandleFunc("/wheel/return", authorized(cfg.DaemonToken, returnWheelHandler(wheelhouse)))

	// Spec §10's "Esquecer". Also not an agent verb, and the only route in this process that deletes
	// a profile a person put logins into.
	profiles, _ := driver.(browser.Profiles)
	mux.HandleFunc("/forget", authorized(cfg.DaemonToken, forgetHandler(profiles)))

	server := &http.Server{
		Addr:              cfg.Addr,
		Handler:           mux,
		ReadHeaderTimeout: HeaderTimeout,
	}
	log.Printf("serving browser on %s with driver %s", cfg.Addr, driver.Name())
	return server.ListenAndServe()
}

// OpenRequest is what the núcleo posts to /open.
//
// The placement is browser.Placement itself rather than a wire type of its own. Everything else here
// is restated deliberately — a wire shape that follows an internal struct around is a wire shape
// nobody decided — but this one is the núcleo's decision travelling verbatim, and a second spelling
// of it would be a second thing to keep in step with `browser_policy.rs`. There is already one such
// mirror across the language boundary; two would be one too many.
type OpenRequest struct {
	URL       string            `json:"url"`
	Placement browser.Placement `json:"placement"`
}

// SessionRequest names an existing session. Used by every verb after /open.
type SessionRequest struct {
	SessionID string `json:"session_id"`
	// ChangesOnly asks a snapshot for what moved since the last one instead of the whole page.
	// Absent means the whole page, which is the answer that is never wrong: an agent that has lost
	// track of what it saw last gets a reading it can act on rather than a difference against
	// something it no longer remembers.
	ChangesOnly bool `json:"changes_only,omitempty"`
	// TextFrom resumes a page's prose where the last snapshot stopped. Absent means the beginning,
	// which is where an agent that has not been cut off yet always is.
	TextFrom int `json:"text_from,omitempty"`
	// ControlsFrom resumes the actionable elements the same way.
	//
	// Its absence here was not a missing feature, it was a broken promise. The driver bounded
	// controls and offered `controls_next` to read on; the núcleo sent `controls_from` back; and
	// this struct had no field for it, so `encoding/json` dropped it and the snapshot restarted at
	// the first control every time. An agent following the offer got page one, forever, with
	// nothing anywhere reporting a problem — the offer was real, the answer was real, and only the
	// relation between them was missing.
	ControlsFrom int `json:"controls_from,omitempty"`
	// Find keeps only what says this, and is how a large page stops costing a whole turn to search.
	// Absent means the whole page.
	Find string `json:"find,omitempty"`
}

// ActRequest is one action against a session.
type ActRequest struct {
	SessionID string `json:"session_id"`
	Kind      string `json:"kind"`
	Ref       string `json:"ref"`
	Text      string `json:"text,omitempty"`
	// Filename is upload's second argument. A field a wire shape is missing is the one thing that
	// fails in total silence — the sender fills it, the decoder finds no home for it, and the
	// driver answers a request nobody made. That happened once already on this struct, with
	// `controls_from`, and it is why `browser.Fake` records the whole request rather than the
	// pieces a caller happened to check.
	Filename string `json:"filename,omitempty"`
}

// HandoffRequest asks for the session to be made ready for a person.
type HandoffRequest struct {
	SessionID string `json:"session_id"`
	Reason    string `json:"reason"`
}

func openHandler(driver browser.Driver) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request OpenRequest
		if !decode(w, r, &request) {
			return
		}
		if strings.TrimSpace(request.URL) == "" {
			http.Error(w, "url is required", http.StatusBadRequest)
			return
		}
		if err := request.Placement.Profile.Validate(); err != nil {
			// Refused at the door, like an unknown action kind. A request that does not say which
			// profile it belongs to cannot be answered by guessing: one guess loses the person's
			// logins and the other hands them to a stranger's page (spec §5.1).
			http.Error(w, "placement: "+err.Error(), http.StatusBadRequest)
			return
		}
		session, err := driver.Open(r.Context(), browser.OpenRequest{
			URL:       request.URL,
			Placement: request.Placement,
		})
		if err != nil {
			writeDriverError(w, "open", err)
			return
		}
		writeJSON(w, session)
	}
}

func snapshotHandler(driver browser.Driver) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request SessionRequest
		if !decode(w, r, &request) {
			return
		}
		if request.SessionID == "" {
			http.Error(w, "session_id is required", http.StatusBadRequest)
			return
		}
		snapshot, err := driver.Snapshot(r.Context(), browser.SessionID(request.SessionID), browser.SnapshotRequest{
			ChangesOnly:  request.ChangesOnly,
			TextFrom:     request.TextFrom,
			ControlsFrom: request.ControlsFrom,
			Find:         request.Find,
		})
		if err != nil {
			writeDriverError(w, "snapshot", err)
			return
		}
		writeJSON(w, snapshot)
	}
}

func actHandler(driver browser.Driver) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request ActRequest
		if !decode(w, r, &request) {
			return
		}
		if request.SessionID == "" {
			http.Error(w, "session_id is required", http.StatusBadRequest)
			return
		}
		kind, ok := parseKind(request.Kind)
		if !ok {
			// A closed vocabulary, refused at the door. An unknown verb must not reach a driver
			// that might interpret it generously (spec §6.2: consequence-free in v1).
			http.Error(w, "unknown action kind: expected click, type, scroll, select, press, back, goto or upload", http.StatusBadRequest)
			return
		}
		result, err := driver.Act(r.Context(), browser.SessionID(request.SessionID), browser.Action{
			Kind:     kind,
			Ref:      request.Ref,
			Text:     request.Text,
			Filename: request.Filename,
		})
		if err != nil {
			writeDriverError(w, "act", err)
			return
		}
		if !result.Valid() {
			// A driver that answers neither done nor properly refused has a bug, and passing it on
			// would tell the agent nothing at all.
			log.Printf("driver returned an invalid act result: %+v", result)
			http.Error(w, "driver returned an invalid result", http.StatusBadGateway)
			return
		}
		// 200 even when refused. See the package comment.
		writeJSON(w, result)
	}
}

func screenshotHandler(driver browser.Driver) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request SessionRequest
		if !decode(w, r, &request) {
			return
		}
		if request.SessionID == "" {
			http.Error(w, "session_id is required", http.StatusBadRequest)
			return
		}
		image, err := driver.Screenshot(r.Context(), browser.SessionID(request.SessionID))
		if err != nil {
			writeDriverError(w, "screenshot", err)
			return
		}
		w.Header().Set("Content-Type", "image/png")
		if _, err := w.Write(image); err != nil {
			log.Printf("writing screenshot: %v", err)
		}
	}
}

// lookHandler answers the AGENT's picture, and answers it as JSON rather than as image bytes.
//
// The difference from /screenshot beside it is not a style choice. That one hands a person's window
// a PNG and has nothing else to say; this one carries the labels as well, and the labels are the
// half that makes the picture actionable — an image body with the refs in a header would be the same
// answer split across two places, one of which nothing else in this sidecar uses.
func lookHandler(driver browser.Driver) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request SessionRequest
		if !decode(w, r, &request) {
			return
		}
		if request.SessionID == "" {
			http.Error(w, "session_id is required", http.StatusBadRequest)
			return
		}
		result, err := driver.Look(r.Context(), browser.SessionID(request.SessionID))
		if err != nil {
			writeDriverError(w, "look", err)
			return
		}
		writeJSON(w, result)
	}
}

func handoffHandler(driver browser.Driver) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request HandoffRequest
		if !decode(w, r, &request) {
			return
		}
		if request.SessionID == "" {
			http.Error(w, "session_id is required", http.StatusBadRequest)
			return
		}
		ticket, err := driver.Handoff(r.Context(), browser.SessionID(request.SessionID), request.Reason)
		if err != nil {
			writeDriverError(w, "handoff", err)
			return
		}
		writeJSON(w, ticket)
	}
}

func closeHandler(driver browser.Driver) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request SessionRequest
		if !decode(w, r, &request) {
			return
		}
		if request.SessionID == "" {
			http.Error(w, "session_id is required", http.StatusBadRequest)
			return
		}
		if err := driver.Close(r.Context(), browser.SessionID(request.SessionID)); err != nil {
			writeDriverError(w, "close", err)
			return
		}
		w.WriteHeader(http.StatusNoContent)
	}
}

// TakeWheelRequest is the núcleo saying a person accepted.
//
// The placement is here for the same reason it is on /open, and one more: the profile a handover
// targets is NOT the one the agent's session was in (spec §4.5), so it cannot be inferred from the
// session id. It has to be sent, and it has to be the núcleo that sends it.
type TakeWheelRequest struct {
	SessionID string            `json:"session_id"`
	URL       string            `json:"url"`
	Placement browser.Placement `json:"placement"`
}

func takeWheelHandler(wheelhouse browser.Wheelhouse) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request TakeWheelRequest
		if !decode(w, r, &request) {
			return
		}
		if wheelhouse == nil {
			http.Error(w, "this driver cannot hand over the wheel", http.StatusNotImplemented)
			return
		}
		if strings.TrimSpace(request.URL) == "" {
			http.Error(w, "url is required", http.StatusBadRequest)
			return
		}
		if err := request.Placement.Profile.Validate(); err != nil {
			http.Error(w, "placement: "+err.Error(), http.StatusBadRequest)
			return
		}
		wheel, err := wheelhouse.TakeWheel(r.Context(), browser.WheelRequest{
			Session:   browser.SessionID(request.SessionID),
			URL:       request.URL,
			Placement: request.Placement,
		})
		if err != nil {
			writeDriverError(w, "wheel/take", err)
			return
		}
		writeJSON(w, wheel)
	}
}

func returnWheelHandler(wheelhouse browser.Wheelhouse) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request SessionRequest
		if !decode(w, r, &request) {
			return
		}
		if wheelhouse == nil {
			http.Error(w, "this driver cannot hand over the wheel", http.StatusNotImplemented)
			return
		}
		if request.SessionID == "" {
			http.Error(w, "session_id is required", http.StatusBadRequest)
			return
		}
		returned, err := wheelhouse.ReturnWheel(r.Context(), browser.SessionID(request.SessionID))
		if err != nil {
			writeDriverError(w, "wheel/return", err)
			return
		}
		writeJSON(w, returned)
	}
}

// ForgetRequest names the profile to delete. A `profile.Ref` and not a project id, so that this
// route cannot be the one place in the system that invents a profile name of its own.
type ForgetRequest struct {
	Profile profile.Ref `json:"profile"`
}

func forgetHandler(profiles browser.Profiles) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		var request ForgetRequest
		if !decode(w, r, &request) {
			return
		}
		if profiles == nil {
			http.Error(w, "this driver does not own any profiles", http.StatusNotImplemented)
			return
		}
		if err := request.Profile.Validate(); err != nil {
			http.Error(w, "profile: "+err.Error(), http.StatusBadRequest)
			return
		}
		stopped, err := profiles.Forget(r.Context(), request.Profile)
		if err != nil {
			writeDriverError(w, "forget", err)
			return
		}
		writeJSON(w, map[string]any{"stopped": stopped})
	}
}

func parseKind(raw string) (browser.ActionKind, bool) {
	switch kind := browser.ActionKind(raw); kind {
	case browser.ActionClick, browser.ActionType, browser.ActionScroll,
		browser.ActionSelect, browser.ActionPress, browser.ActionBack, browser.ActionGoto,
		browser.ActionUpload:
		return kind, true
	default:
		return "", false
	}
}

func decode(w http.ResponseWriter, r *http.Request, into any) bool {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return false
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, MaxBody)).Decode(into); err != nil {
		http.Error(w, "bad request body", http.StatusBadRequest)
		return false
	}
	return true
}

// writeDriverError maps the driver's named failures onto status codes the núcleo can act on.
//
// ErrFenceNotAttached is 503 and not 500 on purpose: nothing is broken, the fence is simply not up,
// and the correct behaviour is to refuse to browse until it is (spec §6.2a). A 500 would read as a
// crash and invite a retry loop against an unfenced browser.
func writeDriverError(w http.ResponseWriter, verb string, err error) {
	switch {
	case errors.Is(err, browser.ErrNoSuchSession):
		http.Error(w, "no such session", http.StatusNotFound)
	case errors.Is(err, browser.ErrFenceNotAttached):
		http.Error(w, "fence is not attached: refusing to browse", http.StatusServiceUnavailable)
	case errors.Is(err, browser.ErrUnsupported):
		http.Error(w, "unsupported by this driver", http.StatusNotImplemented)
	case errors.Is(err, browser.ErrNotInstalled):
		// 503 with the REASON in the body, which is spec §9.5's distinction: 501 would say this build
		// cannot browse, and what is true is that this machine cannot browse yet. The text names the
		// revision and the path, so "still downloading" and "the download keeps failing" are
		// different sentences rather than the same silence.
		http.Error(w, err.Error(), http.StatusServiceUnavailable)
	case errors.Is(err, browser.ErrPersonIsDriving):
		// 409 and not 403: nothing is wrong with the request, and it may well succeed later. The
		// wheel is with a person, and spec §4.4 rule 2 puts no bound on how long that lasts.
		http.Error(w, "a person is driving this profile", http.StatusConflict)
	case errors.Is(err, browser.ErrNoWheelToReturn):
		http.Error(w, "this session is not a person's to give back", http.StatusConflict)
	case errors.Is(err, browser.ErrNotAProjectProfile):
		http.Error(w, "the wheel is only handed over into a project profile", http.StatusBadRequest)
	default:
		log.Printf("%s failed: %v", verb, err)
		http.Error(w, verb+" failed", http.StatusBadGateway)
	}
}

// authorized wraps a handler with the bearer check. Constant-time, like the web sidecar's.
func authorized(token string, next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !hasToken(r, token) {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		next(w, r)
	}
}

func hasToken(r *http.Request, token string) bool {
	const prefix = "Bearer "
	header := r.Header.Get("Authorization")
	if !strings.HasPrefix(header, prefix) {
		return false
	}
	presented := strings.TrimPrefix(header, prefix)
	return subtle.ConstantTimeCompare([]byte(presented), []byte(token)) == 1
}

func writeJSON(w http.ResponseWriter, payload any) {
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(payload); err != nil {
		log.Printf("writing response: %v", err)
	}
}

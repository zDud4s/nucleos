// §spec browser-volante

package serve

import (
	"encoding/json"
	"errors"
	"log"
	"net/http"

	"nucleosbrowser/browser"
)

// writeJSONError answers with {"error": code} and the status. The person routes speak JSON errors
// because core reads the code, unlike the older routes' plain text.
func writeJSONError(w http.ResponseWriter, status int, code string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	if err := json.NewEncoder(w).Encode(map[string]string{"error": code}); err != nil {
		log.Printf("writing error response: %v", err)
	}
}

// decodeJSON reads a POST body into `into`, at most limit bytes. It answers the error itself and
// reports false: 405 for another method, 413 too_large past the limit, 400 bad_request otherwise.
func decodeJSON(w http.ResponseWriter, r *http.Request, into any, limit int64) bool {
	if r.Method != http.MethodPost {
		writeJSONError(w, http.StatusMethodNotAllowed, "method_not_allowed")
		return false
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, limit)).Decode(into); err != nil {
		var tooBig *http.MaxBytesError
		if errors.As(err, &tooBig) {
			writeJSONError(w, http.StatusRequestEntityTooLarge, "too_large")
			return false
		}
		writeJSONError(w, http.StatusBadRequest, "bad_request")
		return false
	}
	return true
}

// answerLimit bounds an /answer body. See the route.
const answerLimit = 14 << 20

// personRequest names the session a person's turn is about.
type personRequest struct {
	Session string `json:"session"`
}

// personRoutes registers /person/begin and /person/end. A driver that cannot seat a person answers 501.
func personRoutes(mux *http.ServeMux, token string, driver browser.Driver) {
	seat, _ := driver.(browser.PersonSeat)
	mux.HandleFunc("/person/begin", authorized(token, func(w http.ResponseWriter, r *http.Request) {
		var request personRequest
		if !decodeJSON(w, r, &request, MaxBody) {
			return
		}
		if seat == nil {
			writeJSONError(w, http.StatusNotImplemented, "unsupported")
			return
		}
		err := seat.BeginPerson(r.Context(), browser.SessionID(request.Session))
		switch {
		case err == nil:
			writeJSON(w, struct{}{})
		case errors.Is(err, browser.ErrNotSoleSession):
			writeJSONError(w, http.StatusConflict, "not_sole_session")
		case errors.Is(err, browser.ErrNoSuchSession):
			writeJSONError(w, http.StatusNotFound, "no_session")
		default:
			writeJSONError(w, http.StatusInternalServerError, err.Error())
		}
	}))
	mux.HandleFunc("/person/end", authorized(token, func(w http.ResponseWriter, r *http.Request) {
		var request personRequest
		if !decodeJSON(w, r, &request, MaxBody) {
			return
		}
		if seat == nil {
			writeJSONError(w, http.StatusNotImplemented, "unsupported")
			return
		}
		returned, err := seat.EndPerson(r.Context(), browser.SessionID(request.Session))
		switch {
		case err == nil:
			chain := returned.Chain
			if chain == nil {
				chain = []string{}
			}
			writeJSON(w, map[string][]string{"chain": chain})
		case errors.Is(err, browser.ErrNotPerson):
			writeJSONError(w, http.StatusConflict, "not_person")
		case errors.Is(err, browser.ErrNoSuchSession):
			writeJSONError(w, http.StatusNotFound, "no_session")
		default:
			writeJSONError(w, http.StatusInternalServerError, err.Error())
		}
	}))
	input, _ := driver.(browser.PersonInput)
	mux.HandleFunc("/input", authorized(token, func(w http.ResponseWriter, r *http.Request) {
		var request struct {
			Session string               `json:"session"`
			Events  []browser.InputEvent `json:"events"`
		}
		if !decodeJSON(w, r, &request, 64*1024) {
			return
		}
		if input == nil {
			writeJSONError(w, http.StatusNotImplemented, "unsupported")
			return
		}
		err := input.Input(r.Context(), browser.SessionID(request.Session), request.Events)
		switch {
		case err == nil:
			writeJSON(w, struct{}{})
		case errors.Is(err, browser.ErrBadEvent):
			writeJSONError(w, http.StatusBadRequest, "bad_event")
		case errors.Is(err, browser.ErrNotPerson):
			writeJSONError(w, http.StatusConflict, "not_person")
		case errors.Is(err, browser.ErrNoSuchSession):
			writeJSONError(w, http.StatusNotFound, "no_session")
		default:
			writeJSONError(w, http.StatusInternalServerError, err.Error())
		}
	}))
	answerer, _ := driver.(browser.PersonAnswer)
	mux.HandleFunc("/answer", authorized(token, func(w http.ResponseWriter, r *http.Request) {
		var request struct {
			Session string          `json:"session"`
			Prompt  string          `json:"prompt"`
			Answer  json.RawMessage `json:"answer"`
		}
		// 14 MiB: a file answer is up to 10 MiB, which is a little over 13.4 MiB in base64.
		if !decodeJSON(w, r, &request, answerLimit) {
			return
		}
		if answerer == nil {
			writeJSONError(w, http.StatusNotImplemented, "unsupported")
			return
		}
		err := answerer.Answer(r.Context(), browser.SessionID(request.Session), request.Prompt, request.Answer)
		switch {
		case err == nil:
			writeJSON(w, struct{}{})
		case errors.Is(err, browser.ErrNoPrompt):
			writeJSONError(w, http.StatusNotFound, "no_prompt")
		case errors.Is(err, browser.ErrNotPerson):
			writeJSONError(w, http.StatusConflict, "not_person")
		case errors.Is(err, browser.ErrBadAnswer):
			writeJSONError(w, http.StatusBadRequest, "bad_answer")
		case errors.Is(err, browser.ErrNoSuchSession):
			writeJSONError(w, http.StatusNotFound, "no_session")
		default:
			writeJSONError(w, http.StatusInternalServerError, err.Error())
		}
	}))
}

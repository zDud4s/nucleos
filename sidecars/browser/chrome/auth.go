// §spec browser-volante

package chrome

import (
	"context"
	"encoding/json"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// This file answers the HTTP-auth challenges the fence asked for. Fetch.enable carries
// handleAuthRequests from Connect, so a 401 no longer falls through to a credential sheet nobody sees:
// the request pauses and the question is ours.
//
// With nobody driving the answer is CancelAuth, which is what a pass-through amounted to before. While
// a person drives it is an auth prompt, answered by continueWithAuth. The credentials the person types
// travel from their answer to that one call and nowhere else: not into a log, not into a record, not
// into an error.

// authRequired is the part of Fetch.authRequired the driver reads.
type authRequired struct {
	RequestID     string `json:"requestId"`
	FrameID       string `json:"frameId"`
	AuthChallenge struct {
		Origin string `json:"origin"`
		Realm  string `json:"realm"`
	} `json:"authChallenge"`
}

// onAuthRequired answers or raises a challenge. Like onFetchPaused it runs on the dispatch goroutine,
// so in person mode it makes no call: it files the prompt and returns.
func (d *Driver) onAuthRequired(event cdp.Event) {
	if event.Method != "Fetch.authRequired" {
		return
	}
	var challenge authRequired
	if err := json.Unmarshal(event.Params, &challenge); err != nil || challenge.RequestID == "" {
		return
	}
	// on is the session the event came on: Fetch.enable lives on the browser session, so that is where
	// Chrome expects the answer. It is not the page's session, so the challenge is matched to the person
	// by frame, as the fence does for a document request.
	on := event.Session
	requestID := challenge.RequestID

	if state := d.person.Load(); state != nil && state.frame != "" && challenge.FrameID == state.frame {
		// The prompt is shown on the person's page, where their viewer listens; the answer goes back
		// on the session the challenge arrived on, carried as the prompt's target.
		raised := d.raisePromptFor(state, state.page, browser.Prompt{
			Kind:   "auth",
			Origin: challenge.AuthChallenge.Origin,
			Realm:  challenge.AuthChallenge.Realm,
		}, func(ctx context.Context) {
			d.answerAuth(ctx, on, requestID, map[string]any{"response": "CancelAuth"})
		}, requestID, on)
		if raised {
			return
		}
	}

	ctx, cancel := context.WithTimeout(context.Background(), fenceCallTimeout)
	defer cancel()
	d.answerAuth(ctx, on, requestID, map[string]any{"response": "CancelAuth"})
}

// answerAuth sends one Fetch.continueWithAuth. Its error is dropped on purpose: the only thing it
// could carry worth saying is the response, and that may hold a password.
func (d *Driver) answerAuth(ctx context.Context, on cdp.SessionID, requestID string, response map[string]any) {
	_, _ = d.conn.Call(ctx, on, "Fetch.continueWithAuth", map[string]any{
		"requestId":             requestID,
		"authChallengeResponse": response,
	})
}

// answerAuthPrompt validates and delivers a person's answer to an auth prompt. The body is checked
// before the prompt is taken, and a bad one is reported bare: ErrBadAnswer never quotes it.
func (d *Driver) answerAuthPrompt(ctx context.Context, state *personState, pending *pendingPrompt, promptID string, answer json.RawMessage) error {
	var reply struct {
		Cancel   bool   `json:"cancel"`
		Username string `json:"username"`
		Password string `json:"password"`
	}
	if err := json.Unmarshal(answer, &reply); err != nil {
		return browser.ErrBadAnswer
	}
	if d.takePrompt(state, promptID) == nil {
		return browser.ErrNoPrompt
	}
	response := map[string]any{"response": "CancelAuth"}
	if !reply.Cancel {
		response = map[string]any{
			"response": "ProvideCredentials",
			"username": reply.Username,
			"password": reply.Password,
		}
	}
	// The call's error is not returned: it could echo the params.
	answerOn, _ := pending.target.(cdp.SessionID)
	_, _ = d.conn.Call(ctx, answerOn, "Fetch.continueWithAuth", map[string]any{
		"requestId":             pending.requestID,
		"authChallengeResponse": response,
	})
	return nil
}

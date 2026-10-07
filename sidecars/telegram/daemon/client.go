package daemon

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"strconv"
	"time"

	"nucleostelegram/notifier"
)

type Client struct {
	baseURL string
	token   string
	http    *http.Client
}

func New(baseURL, token string) *Client {
	return &Client{
		baseURL: baseURL,
		token:   token,
		http: &http.Client{
			Timeout: 30 * time.Second,
		},
	}
}

func (c *Client) do(method, path string, body any) ([]byte, int, error) {
	var requestBody io.Reader
	if body != nil {
		encoded, err := json.Marshal(body)
		if err != nil {
			return nil, 0, fmt.Errorf("encode request body: %w", err)
		}
		requestBody = bytes.NewReader(encoded)
	}

	req, err := http.NewRequest(method, c.baseURL+path, requestBody)
	if err != nil {
		return nil, 0, fmt.Errorf("create request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("Content-Type", "application/json")

	resp, err := c.http.Do(req)
	if err != nil {
		return nil, 0, fmt.Errorf("perform request: %w", err)
	}
	defer resp.Body.Close()

	responseBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, resp.StatusCode, fmt.Errorf("read response: %w", err)
	}

	return responseBody, resp.StatusCode, nil
}

func (c *Client) SendAssistantMessage(chatID, text string) (int64, error) {
	// The daemon routes a turn on this, so it is stated rather than left to be inferred from the
	// shape of chatID. A Telegram group id is negative, which makes it guessable — and would make
	// the routing depend on a numbering scheme Telegram owns and can change. An older daemon that
	// does not know the field ignores it, so this is safe to send before the other side ships.
	body, status, err := c.do(http.MethodPost, "/assistant/message", map[string]string{
		"chat_id": chatID,
		"text":    text,
		"origin":  "telegram",
	})
	if err != nil {
		return 0, fmt.Errorf("send assistant message: %w", err)
	}
	if err := statusError("send assistant message", status, body); err != nil {
		return 0, err
	}

	var response struct {
		TurnID int64 `json:"turn_id"`
	}
	if err := json.Unmarshal(body, &response); err != nil {
		return 0, fmt.Errorf("parse assistant message response: %w", err)
	}
	return response.TurnID, nil
}

// CreateNote saves an owner note. Errors never carry text: a note is the owner's private writing.
func (c *Client) CreateNote(text string) (int64, error) {
	body, status, err := c.do(http.MethodPost, "/owner-notes", map[string]string{
		"text":   text,
		"origin": "telegram",
	})
	if err != nil {
		return 0, fmt.Errorf("create note: %w", err)
	}
	if err := statusError("create note", status, body); err != nil {
		return 0, err
	}

	var response struct {
		ID int64 `json:"id"`
	}
	if err := json.Unmarshal(body, &response); err != nil {
		return 0, fmt.Errorf("parse create note response: %w", err)
	}
	return response.ID, nil
}

// AnswerCapture answers the capture request of a job with the owner's text. released is false when
// the request had already closed: the note is kept anyway, the distiller has simply moved on.
// Errors never carry text, for the same reason as CreateNote.
func (c *Client) AnswerCapture(jobID int64, text string) (int64, bool, error) {
	path := fmt.Sprintf("/capture-requests/%d/answer", jobID)
	body, status, err := c.do(http.MethodPost, path, map[string]string{
		"text":   text,
		"origin": "telegram",
	})
	if err != nil {
		return 0, false, fmt.Errorf("answer capture: %w", err)
	}
	if err := statusError("answer capture", status, body); err != nil {
		return 0, false, err
	}

	var response struct {
		NoteID   int64 `json:"note_id"`
		Released bool  `json:"released"`
	}
	if err := json.Unmarshal(body, &response); err != nil {
		return 0, false, fmt.Errorf("parse answer capture response: %w", err)
	}
	return response.NoteID, response.Released, nil
}

func (c *Client) GetRun(id int64) (map[string]any, error) {
	return c.getObject("get run", "/assistant/"+strconv.FormatInt(id, 10))
}

func (c *Client) GetProposals() ([]map[string]any, error) {
	body, status, err := c.do(http.MethodGet, "/proposals", nil)
	if err != nil {
		return nil, fmt.Errorf("get proposals: %w", err)
	}
	if err := statusError("get proposals", status, body); err != nil {
		return nil, err
	}

	var proposals []map[string]any
	if err := json.Unmarshal(body, &proposals); err != nil {
		return nil, fmt.Errorf("parse proposals response: %w", err)
	}
	return proposals, nil
}

// GetRefusedActions reads what the injection barrier turned away and nobody has put away yet.
//
// Its own route rather than a filter on `/proposals`: that list feeds approve and reject, and both
// answer 409 for anything that is not an action-approval. A refused action was never held — the
// turn was denied and carried on — so there is nothing to let through and nothing to release.
func (c *Client) GetRefusedActions() ([]map[string]any, error) {
	body, status, err := c.do(http.MethodGet, "/proposals/refused-actions", nil)
	if err != nil {
		return nil, fmt.Errorf("get refused actions: %w", err)
	}
	if err := statusError("get refused actions", status, body); err != nil {
		return nil, err
	}

	var refused []map[string]any
	if err := json.Unmarshal(body, &refused); err != nil {
		return nil, fmt.Errorf("parse refused actions response: %w", err)
	}
	return refused, nil
}

func (c *Client) GetProjects() ([]map[string]any, error) {
	body, status, err := c.do(http.MethodGet, "/projects", nil)
	if err != nil {
		return nil, fmt.Errorf("get projects: %w", err)
	}
	if err := statusError("get projects", status, body); err != nil {
		return nil, err
	}

	var projects []map[string]any
	if err := json.Unmarshal(body, &projects); err != nil {
		return nil, fmt.Errorf("parse projects response: %w", err)
	}
	return projects, nil
}

func (c *Client) ApproveProposal(id int64) (map[string]any, error) {
	return c.postObject("approve proposal", "/proposals/"+strconv.FormatInt(id, 10)+"/approve", nil)
}

func (c *Client) RejectProposal(id int64) error {
	return c.postNoContent("reject proposal", "/proposals/"+strconv.FormatInt(id, 10)+"/reject", nil)
}

func (c *Client) GetFeed() ([]map[string]any, error) {
	body, status, err := c.do(http.MethodGet, "/feed?scope=all", nil)
	if err != nil {
		return nil, fmt.Errorf("get feed: %w", err)
	}
	if err := statusError("get feed", status, body); err != nil {
		return nil, err
	}

	var feed []map[string]any
	if err := json.Unmarshal(body, &feed); err != nil {
		return nil, fmt.Errorf("parse feed response: %w", err)
	}
	return feed, nil
}

// GetNotifyPolicy is which feed kinds the owner still wants forwarded. Read once per notifier
// round, beside GetFeed, and never cached: the policy is a handful of rows, the round already
// makes five calls, and a cache is the difference between a switch that works when you flip it and
// one that works a while later.
//
// The caller decides what a failure means, and in the notifier it means everything passes — the
// mechanism guards against noise, so its own failure must not be silence.
func (c *Client) GetNotifyPolicy() (notifier.Policy, error) {
	var policy notifier.Policy

	body, status, err := c.do(http.MethodGet, "/notifications/policy", nil)
	if err != nil {
		return policy, fmt.Errorf("get notify policy: %w", err)
	}
	if err := statusError("get notify policy", status, body); err != nil {
		return policy, err
	}

	if err := json.Unmarshal(body, &policy); err != nil {
		return notifier.Policy{}, fmt.Errorf("parse notify policy response: %w", err)
	}
	return policy, nil
}

func (c *Client) GetBudget() (map[string]any, error) {
	return c.getObject("get budget", "/autopilot/budget")
}

// TriageEmail asks the núcleo to classify whatever mail is waiting. Collecting mail is free and
// happens on its own; this is the part that costs a run, so it only happens when asked.
func (c *Client) TriageEmail() (map[string]any, error) {
	body, status, err := c.do(http.MethodPost, "/email/triage", nil)
	if err != nil {
		return nil, fmt.Errorf("triage email: %w", err)
	}
	if status < 200 || status >= 300 {
		return nil, fmt.Errorf("triage email: daemon returned %d", status)
	}
	var out map[string]any
	if err := json.Unmarshal(body, &out); err != nil {
		return nil, fmt.Errorf("triage email: %w", err)
	}
	return out, nil
}

// GetEmailQueue shows what the pillar knows, and costs nothing.
func (c *Client) GetEmailQueue() ([]map[string]any, error) {
	body, status, err := c.do(http.MethodGet, "/email/queue", nil)
	if err != nil {
		return nil, fmt.Errorf("get email queue: %w", err)
	}
	if status < 200 || status >= 300 {
		return nil, fmt.Errorf("get email queue: daemon returned %d", status)
	}
	var out []map[string]any
	if err := json.Unmarshal(body, &out); err != nil {
		return nil, fmt.Errorf("get email queue: %w", err)
	}
	return out, nil
}

func (c *Client) GetKill() (bool, error) {
	body, status, err := c.do(http.MethodGet, "/autopilot/kill", nil)
	if err != nil {
		return false, fmt.Errorf("get kill state: %w", err)
	}
	if err := statusError("get kill state", status, body); err != nil {
		return false, err
	}

	var response struct {
		Engaged bool `json:"engaged"`
	}
	if err := json.Unmarshal(body, &response); err != nil {
		return false, fmt.Errorf("parse kill state response: %w", err)
	}
	return response.Engaged, nil
}

func (c *Client) SetKill(engaged bool) error {
	return c.postNoContent("set kill state", "/autopilot/kill", map[string]bool{"engaged": engaged})
}

func (c *Client) CancelRun(id int64) error {
	return c.postNoContent("cancel run", "/runs/"+strconv.FormatInt(id, 10)+"/cancel", nil)
}

func (c *Client) getObject(operation, path string) (map[string]any, error) {
	body, status, err := c.do(http.MethodGet, path, nil)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", operation, err)
	}
	if err := statusError(operation, status, body); err != nil {
		return nil, err
	}

	var response map[string]any
	if err := json.Unmarshal(body, &response); err != nil {
		return nil, fmt.Errorf("parse %s response: %w", operation, err)
	}
	return response, nil
}

func (c *Client) postObject(operation, path string, request any) (map[string]any, error) {
	body, status, err := c.do(http.MethodPost, path, request)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", operation, err)
	}
	if err := statusError(operation, status, body); err != nil {
		return nil, err
	}

	var response map[string]any
	if err := json.Unmarshal(body, &response); err != nil {
		return nil, fmt.Errorf("parse %s response: %w", operation, err)
	}
	return response, nil
}

func (c *Client) postNoContent(operation, path string, request any) error {
	body, status, err := c.do(http.MethodPost, path, request)
	if err != nil {
		return fmt.Errorf("%s: %w", operation, err)
	}
	return statusError(operation, status, body)
}

// StatusError is a refusal the núcleo stated, kept as data rather than folded into a sentence.
//
// Refusal is the núcleo's own name for what it refused, and it exists because the status code is
// not enough: `/assistant/message` alone refuses several different ways, and two of them are 409.
// A chat mid-turn clears by waiting; a missing local model never clears on its own. A
// caller holding only the number has to guess, and the cheap guess leaves a topic silent with an
// explanation that was never true.
//
// It is empty for any refusal that arrived without one — an older núcleo, a route that does not
// name them, an HTML error page from something in between. That is a normal answer and not a parse
// failure: the caller falls back to reporting what it has. Turning a stated refusal into a broken
// client would be strictly worse than saying less about it.
type StatusError struct {
	Operation string
	Status    int
	Refusal   string
	Body      string
}

func (e *StatusError) Error() string {
	return fmt.Sprintf("%s: status code %d: %s", e.Operation, e.Status, e.Body)
}

func statusError(operation string, status int, body []byte) error {
	if status >= http.StatusOK && status < http.StatusMultipleChoices {
		return nil
	}
	// Decoded best-effort and never checked: see the type's note on why an unnamed refusal is an
	// answer. A body that is not JSON leaves Refusal empty, which is exactly the fallback.
	var named struct {
		Refusal string `json:"refusal"`
	}
	_ = json.Unmarshal(body, &named)
	return &StatusError{
		Operation: operation,
		Status:    status,
		Refusal:   named.Refusal,
		Body:      string(bytes.TrimSpace(body)),
	}
}

// VoiceTranscriber is the daemon's voice pillar, when it is armed.
//
// Split out as an interface so the pipe can be tested against a stub, and so a daemon that predates
// the pillar (or has it switched off) is a normal, expected answer rather than an error path.
type VoiceTranscriber interface {
	VoiceCapture(audio []byte, format string, durationMs int64) (string, bool, error)
}

// VoiceCapture sends a recording to the núcleo and returns the cleaned transcript.
//
// This is what stops the sidecar being a second implementation of the STT contract. The núcleo already
// spawns a transcriber, applies the operator's misheard-word hints, runs the local cleanup model and
// keeps the result — none of which this process can do, and all of which a voice note deserves as much
// as a dictation does.
//
// The second return value is "the daemon can do this", NOT "it worked". It is false when the pillar is
// off (503) or refusing connections, which is the case the caller must be able to distinguish: those mean fall
// back to the local command, while a genuine failure means say so. A voice note is somebody talking to
// you, and it must not stop arriving because an optional pillar is not configured.
//
// Sent as raw bytes with the container named in the query string rather than as base64 in JSON: base64
// would inflate every recording by a third for nothing, and the daemon names its temp file from the
// container so the transcriber knows how to decode it.
func (c *Client) VoiceCapture(audio []byte, format string, durationMs int64) (string, bool, error) {
	url := fmt.Sprintf(
		"%s/voice/capture?kind=dictation&duration_ms=%d&format=%s",
		c.baseURL, durationMs, format,
	)
	req, err := http.NewRequest(http.MethodPost, url, bytes.NewReader(audio))
	if err != nil {
		return "", false, fmt.Errorf("create voice request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("Content-Type", "application/octet-stream")

	resp, err := c.http.Do(req)
	if err != nil {
		// Only a failure to CONNECT means "nothing is listening", and only that lets the local command
		// take over: no request reached the daemon, so no work was started. Any other transport error
		// (the client timeout above all) is a daemon that accepted the recording and did not answer in
		// time; reading it as "voice is off" would transcribe the note a second time and hide that the
		// daemon is struggling. It comes back as an error with `true`, so the caller says so.
		var opErr *net.OpError
		if errors.As(err, &opErr) && opErr.Op == "dial" && !opErr.Timeout() {
			return "", false, nil
		}
		return "", true, fmt.Errorf("voice capture: daemon did not answer: %w", err)
	}
	defer resp.Body.Close()

	switch {
	case resp.StatusCode == http.StatusServiceUnavailable:
		// The daemon's own words for "no transcriber is configured; voice is off".
		return "", false, nil
	case resp.StatusCode == http.StatusNoContent:
		// It ran, and heard nothing. A real answer, and not one the local command would improve on.
		return "", true, nil
	case resp.StatusCode < 200 || resp.StatusCode >= 300:
		body, _ := io.ReadAll(io.LimitReader(resp.Body, 512))
		return "", true, fmt.Errorf("voice capture failed (%d): %s", resp.StatusCode, bytes.TrimSpace(body))
	}

	var answer struct {
		Text string `json:"text"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&answer); err != nil {
		return "", true, fmt.Errorf("decode voice capture answer: %w", err)
	}
	return answer.Text, true, nil
}

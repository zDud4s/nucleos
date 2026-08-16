package daemon

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"time"
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
// not enough: `/assistant/message` alone refuses four different ways across three codes, and two of
// them are 409. A chat mid-turn clears by waiting; a paused errand never clears on its own. A
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
// off (503) or unreachable, which is the case the caller must be able to distinguish: those mean fall
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
		// Unreachable is indistinguishable from not-configured for our purposes: either way the local
		// command is the only thing that can still answer.
		return "", false, nil
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

// Errand is one topic that became a place to work, as the daemon reports it.
//
// A struct and not a map[string]any like the other list endpoints here, because every field is
// read by name and typed: a `/pausa` that silently no-oped on a status read out of an `any` would
// look exactly like a `/pausa` that worked.
type Errand struct {
	ID   int64  `json:"id"`
	Name string `json:"name"`
	// ChatKey is the topic it sits on, and the handle the sidecar has: a command typed in a topic
	// knows its key and nothing else.
	ChatKey string `json:"chat_key"`
	Brain   string `json:"brain"`
	Folder  string `json:"folder"`
	Status  string `json:"status"`
}

func (c *Client) ListErrands() ([]Errand, error) {
	body, status, err := c.do(http.MethodGet, "/errands", nil)
	if err != nil {
		return nil, fmt.Errorf("list errands: %w", err)
	}
	if err := statusError("list errands", status, body); err != nil {
		return nil, err
	}
	var errands []Errand
	if err := json.Unmarshal(body, &errands); err != nil {
		return nil, fmt.Errorf("parse list errands response: %w", err)
	}
	return errands, nil
}

// ErrandOfChat is the errand on one topic, and whether there is one.
//
// A list filtered here rather than a route that takes a key, because the daemon's errand routes are
// keyed by id and the sidecar never holds one — it holds the chat key it is standing in. The list
// is short by construction: it is one person's errands, not a queue.
//
// Not-found is `(_, false, nil)` and never an error. Almost no topic has an errand, so the absence
// is the ordinary answer, and reporting it as a failure would put "couldn't reach the daemon" in
// front of somebody who typed `/pausa` in the wrong place.
func (c *Client) ErrandOfChat(chatKey string) (Errand, bool, error) {
	errands, err := c.ListErrands()
	if err != nil {
		return Errand{}, false, err
	}
	for _, errand := range errands {
		if errand.ChatKey == chatKey {
			return errand, true, nil
		}
	}
	return Errand{}, false, nil
}

func (c *Client) CreateErrand(name, chatKey string) (int64, error) {
	response, err := c.postObject("open errand", "/errands", map[string]string{
		"name":     name,
		"chat_key": chatKey,
	})
	if err != nil {
		return 0, err
	}
	id, _ := response["errand_id"].(float64)
	return int64(id), nil
}

// SetErrandStatus pauses or resumes one errand.
//
// The body carries `status` alone. The daemon's PATCH takes both fields optionally, and sending
// both would mean `/pausa` also restating the brain — a command about one thing quietly rewriting
// another, with the rewrite invisible because it usually restates what was already true.
func (c *Client) SetErrandStatus(id int64, status string) error {
	return c.patchNoContent("set errand status", errandPath(id), map[string]string{"status": status})
}

func (c *Client) SetErrandBrain(id int64, brain string) error {
	return c.patchNoContent("set errand brain", errandPath(id), map[string]string{"brain": brain})
}

func (c *Client) CloseErrand(id int64) error {
	body, status, err := c.do(http.MethodDelete, errandPath(id), nil)
	if err != nil {
		return fmt.Errorf("close errand: %w", err)
	}
	return statusError("close errand", status, body)
}

func errandPath(id int64) string {
	return "/errands/" + strconv.FormatInt(id, 10)
}

func (c *Client) patchNoContent(operation, path string, request any) error {
	body, status, err := c.do(http.MethodPatch, path, request)
	if err != nil {
		return fmt.Errorf("%s: %w", operation, err)
	}
	return statusError(operation, status, body)
}

package adapter

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net/http"
	"regexp"
	"strconv"
	"strings"

	"nucleostelegram/telegram"
)

// Capture requests (spec 2026-10-07-pedidos-captura-design, 8.3). The core's request message carries a
// `#cap<job_id>` mark; the owner answers it with Telegram's native reply. The mark travels in the
// replied-to message itself, so the sidecar keeps no state and a restart loses nothing.

// captureMark finds the mark in the text of the replied-to message.
var captureMark = regexp.MustCompile(`#cap(\d+)`)

// Replies to the owner. A dictated answer is refused with the same words as a dictated /note (S10).
const (
	captureRefusal  = "não aceito respostas ditadas, de foto ou de ficheiro: escreve a resposta"
	captureTooLate  = "anotado; o destilador já avançou neste job"
	captureNotSaved = "não consegui anotar esta resposta; tenta de novo"
)

// CaptureAnswer is the core's 201 body.
type CaptureAnswer struct {
	NoteID   int64 `json:"note_id"`
	Released bool  `json:"released"`
}

// ErrCaptureRejected is the core refusing the answer for good (400, 404): retrying cannot help.
var ErrCaptureRejected = errors.New("capture answer rejected")

// CaptureAPI posts an answer to POST /capture-requests/{job_id}/answer. An error that is not
// ErrCaptureRejected is transient (transport, 5xx).
type CaptureAPI interface {
	Answer(ctx context.Context, jobID int64, text string) (CaptureAnswer, error)
}

// CaptureReplier sends the sidecar's one-line acknowledgements; *telegram.Client satisfies it.
type CaptureReplier interface {
	SendHTMLEx(ctx context.Context, p telegram.SendParams) (int64, error)
}

// Capture routes replies to capture requests. A nil *Capture routes nothing.
type Capture struct {
	API CaptureAPI
	Bot CaptureReplier
}

// captureJobID is the job a message answers: it must reply to a message written by a bot whose text
// carries `#cap<job_id>`. is_bot rather than "is our bot": the sidecar never calls getMe. The core
// still checks that the request exists (404).
func captureJobID(msg *telegram.Message) (int64, bool) {
	if msg == nil || msg.From == nil || msg.ReplyToMessage == nil {
		return 0, false
	}
	r := msg.ReplyToMessage
	if r.From == nil || !r.From.IsBot {
		return 0, false
	}
	m := captureMark.FindStringSubmatch(r.Text + "\n" + r.Caption)
	if m == nil {
		return 0, false
	}
	id, err := strconv.ParseInt(m[1], 10, 64)
	if err != nil || id <= 0 {
		return 0, false
	}
	return id, true
}

// handle processes an update when it is a capture reply. handled=false means the update is an ordinary
// message. advance reports whether the offset may move past a handled update. Nothing of the reply's
// text is ever logged.
func (c *Capture) handle(ctx context.Context, u telegram.Update) (handled, advance bool) {
	if c == nil || c.API == nil || u.Message == nil || u.CallbackQuery != nil {
		return false, false
	}
	msg := u.Message
	// Same gate as Inbound: a sender and a chat kind the contract models.
	if _, _, ok := conversationAndSender(msg); !ok {
		return false, false
	}
	jobID, ok := captureJobID(msg)
	if !ok {
		return false, false
	}
	if hasMedia(msg) || msg.Text == "" {
		c.say(ctx, msg, captureRefusal)
		return true, true
	}
	ans, err := c.API.Answer(ctx, jobID, msg.Text)
	switch {
	case err == nil:
		reply := fmt.Sprintf("anotado (#%d), ligado ao job #%d", ans.NoteID, jobID)
		if !ans.Released {
			reply = captureTooLate
		}
		c.say(ctx, msg, reply)
		return true, true
	case errors.Is(err, ErrCaptureRejected):
		log.Printf("capture answer for job %d rejected by the core", jobID)
		c.say(ctx, msg, captureNotSaved)
		return true, true
	default:
		if ctx.Err() == nil {
			log.Printf("capture answer for job %d failed: %v", jobID, err)
		}
		return true, false
	}
}

func (c *Capture) say(ctx context.Context, msg *telegram.Message, text string) {
	if c.Bot == nil {
		return
	}
	_, err := c.Bot.SendHTMLEx(ctx, telegram.SendParams{
		To:               msg.Destination(),
		Text:             text,
		ReplyToMessageID: msg.MessageID,
		Silent:           true,
	})
	if err != nil && ctx.Err() == nil {
		log.Printf("capture acknowledgement not sent: %v", err)
	}
}

// CaptureHTTP is the production CaptureAPI over the core's HTTP API.
type CaptureHTTP struct {
	BaseURL string
	Token   string
	HTTP    *http.Client // nil means http.DefaultClient
}

// Answer implements CaptureAPI.
func (h CaptureHTTP) Answer(ctx context.Context, jobID int64, text string) (CaptureAnswer, error) {
	body, err := json.Marshal(map[string]string{"text": text, "origin": "telegram"})
	if err != nil {
		return CaptureAnswer{}, err
	}
	url := strings.TrimRight(h.BaseURL, "/") + "/capture-requests/" + strconv.FormatInt(jobID, 10) + "/answer"
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
	if err != nil {
		return CaptureAnswer{}, errors.New("create capture request")
	}
	req.Header.Set("Authorization", "Bearer "+h.Token)
	req.Header.Set("Content-Type", "application/json")
	client := h.HTTP
	if client == nil {
		client = http.DefaultClient
	}
	resp, err := client.Do(req)
	if err != nil {
		return CaptureAnswer{}, errors.New("capture request: transport error")
	}
	defer resp.Body.Close()
	switch {
	case resp.StatusCode == http.StatusBadRequest || resp.StatusCode == http.StatusNotFound:
		return CaptureAnswer{}, fmt.Errorf("%w: status %d", ErrCaptureRejected, resp.StatusCode)
	case resp.StatusCode != http.StatusCreated && resp.StatusCode != http.StatusOK:
		return CaptureAnswer{}, fmt.Errorf("capture answer: status %d", resp.StatusCode)
	}
	var out CaptureAnswer
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		return CaptureAnswer{}, errors.New("capture answer: bad response body")
	}
	return out, nil
}

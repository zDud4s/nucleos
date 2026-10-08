package telegram

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

const (
	// A 429 is worth waiting out a couple of times, no more: this process also carries `/kill`, and
	// a long sleep inside a send is a stretch of time in which nothing else here runs.
	rateLimitRetries = 2
	maxRetryAfter    = 30 * time.Second
	// Telegram sends no retry_after with some 429s; waiting a beat still beats hammering it.
	defaultRetryAfter = 3 * time.Second
	// A response is JSON this process parses into memory; a hostile or broken proxy answering with
	// an endless body must not be able to grow this process without bound.
	maxResponseBytes = 8 << 20
	// Attachments are read whole into memory before being written to disk; Telegram's own cap for
	// a bot download is 20 MiB, so anything past that is not a file this bot can be sent.
	maxDownloadBytes = 20 << 20
	tokenPlaceholder = "[redacted]"
)

type Update struct {
	UpdateID      int64          `json:"update_id"`
	Message       *Message       `json:"message"`
	CallbackQuery *CallbackQuery `json:"callback_query"`
}

type Message struct {
	MessageID int64 `json:"message_id"`
	Chat      Chat  `json:"chat"`
	// MessageThreadID is set for forum topics AND for plain reply chains in a supergroup, which is
	// why it is never read on its own — see IsTopicMessage.
	MessageThreadID int64 `json:"message_thread_id"`
	// IsTopicMessage is Telegram saying this really is a forum topic. Reading MessageThreadID
	// without it would give every reply chain in an ordinary group a key of its own, and on the day
	// that ships every one of those conversations loses its session.
	IsTopicMessage bool `json:"is_topic_message"`
	// From is who typed it, which is not the same question as which chat it arrived in: a group
	// chat id authorises a room, and a room's membership changes without anyone reconfiguring
	// this sidecar.
	From *User  `json:"from"`
	Text string `json:"text"`
	// Caption carries the instruction on a photo or a document. Without it the orchestrator is
	// handed a file and no idea what to do with it.
	Caption  string      `json:"caption"`
	Voice    *Voice      `json:"voice"`
	Document *Document   `json:"document"`
	Photo    []PhotoSize `json:"photo"`
	// ReplyToMessage is the message this one answers, when the owner used Telegram's reply.
	// A capture request is answered this way: the request's own text carries its #cap<job_id> mark.
	ReplyToMessage *Message `json:"reply_to_message"`
}

// SenderID is the id of whoever sent the message, or 0 when Telegram sent no `from` (channel posts,
// anonymous group admins). Zero is never an authorised id, so an unattributable message fails shut.
func (m *Message) SenderID() int64 {
	if m == nil || m.From == nil {
		return 0
	}
	return m.From.ID
}

type Voice struct {
	FileID   string `json:"file_id"`
	Duration int    `json:"duration"`
}

type Document struct {
	FileID   string `json:"file_id"`
	FileName string `json:"file_name"`
}

type PhotoSize struct {
	FileID   string `json:"file_id"`
	Width    int    `json:"width"`
	Height   int    `json:"height"`
	FileSize int    `json:"file_size"`
}

type File struct {
	FileID   string `json:"file_id"`
	FilePath string `json:"file_path"`
}

type Chat struct {
	ID int64 `json:"id"`
}

// Destination is where a message goes: the chat, and the forum topic inside it when there is one.
//
// A struct rather than a second int64 parameter on every send. Two adjacent int64s is the signature
// you transpose without the compiler noticing, and the symptom of transposing these two is a reply
// delivered to a topic id used as a chat id — a 400 from Telegram at best, and at worst a message in
// somewhere else entirely. It also keeps the ~forty call sites in `pipe` compiling unchanged: they
// pass one value that already knows both halves.
//
// A zero ThreadID means "no topic" and is omitted from the request rather than sent as 0, which
// Telegram reads as a topic that does not exist.
type Destination struct {
	ChatID   int64
	ThreadID int64
}

// Destination is where a reply to this message belongs.
//
// The topic is taken only when Telegram says it IS a topic. Everything else — a one-to-one chat, a
// group's General, a reply chain in a non-forum supergroup — comes back as the bare chat, which is
// the key this sidecar has always used.
func (m *Message) Destination() Destination {
	if m == nil {
		return Destination{}
	}
	if !m.IsTopicMessage {
		return Destination{ChatID: m.Chat.ID}
	}
	return Destination{ChatID: m.Chat.ID, ThreadID: m.MessageThreadID}
}

// body starts the JSON for a send, with the topic present only when there is one.
func (d Destination) body() map[string]any {
	request := map[string]any{"chat_id": d.ChatID}
	if d.ThreadID != 0 {
		request["message_thread_id"] = d.ThreadID
	}
	return request
}

type CallbackQuery struct {
	ID      string   `json:"id"`
	Data    string   `json:"data"`
	Message *Message `json:"message"`
	From    User     `json:"from"`
}

type User struct {
	ID int64 `json:"id"`
	// IsBot is mandatory in the Bot API. The sidecar never calls getMe, so it does not know its own
	// id; "a bot wrote it" is what identifies a capture request in an authorised chat.
	IsBot bool `json:"is_bot"`
}

type Button struct {
	Text         string
	CallbackData string
}

// APIError is Telegram answering: the request arrived and was refused. Keeping it distinct from a
// transport failure is what lets a caller decide whether re-sending anything can possibly help —
// re-sending into a throttle is what turns a throttle into a longer one.
type APIError struct {
	Method      string
	StatusCode  int
	Description string
	// RetryAfter is non-zero only for 429, and is how long Telegram asked us to stay quiet.
	RetryAfter time.Duration
}

func (e *APIError) Error() string {
	if e.RetryAfter > 0 {
		return fmt.Sprintf("telegram %s: rate limited, retry after %s", e.Method, e.RetryAfter)
	}
	if e.Description != "" {
		return fmt.Sprintf("telegram %s: status code %d: %s", e.Method, e.StatusCode, e.Description)
	}
	return fmt.Sprintf("telegram %s: status code %d", e.Method, e.StatusCode)
}

// IsRateLimited reports whether err is Telegram throttling us.
func IsRateLimited(err error) bool {
	var apiErr *APIError
	return errors.As(err, &apiErr) && apiErr.RetryAfter > 0
}

// IsTransport reports whether err means the request never reached Telegram (DNS, timeout, reset).
// Nothing about the message content caused it, so re-sending different content cannot fix it.
func IsTransport(err error) bool {
	var urlErr *url.Error
	return errors.As(err, &urlErr)
}

// redactedError keeps the original error reachable through errors.Is/As while its message has the
// bot token blanked out. It exists because Go reports transport failures as *url.Error, whose
// Error() prints the full request URL — and this API carries the bot token in the URL path.
type redactedError struct {
	err  error
	text string
}

func (e *redactedError) Error() string { return e.text }
func (e *redactedError) Unwrap() error { return e.err }

type Client struct {
	token   string
	apiBase string
	http    *http.Client
	// sleep is a field so a test can wait out a 429 without actually waiting.
	sleep func(ctx context.Context, d time.Duration) bool
}

func New(token string) *Client {
	return &Client{
		token:   token,
		apiBase: "https://api.telegram.org",
		http: &http.Client{
			Timeout: 65 * time.Second,
		},
		sleep: waitFor,
	}
}

func waitFor(ctx context.Context, d time.Duration) bool {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}

// redact strips the bot token out of an error message. Every error leaving this client goes through
// it, because every one of them can carry a URL, and a logged URL is a leaked token.
func (c *Client) redact(err error) error {
	if err == nil || c.token == "" {
		return err
	}
	text := err.Error()
	cleaned := strings.ReplaceAll(text, c.token, tokenPlaceholder)
	if cleaned == text {
		return err
	}
	return &redactedError{err: err, text: cleaned}
}

// call POSTs a Bot API method with a JSON body and returns the raw result field.
func (c *Client) call(method string, body any) (json.RawMessage, error) {
	return c.callContext(context.Background(), method, body)
}

func (c *Client) callContext(ctx context.Context, method string, body any) (json.RawMessage, error) {
	encoded, err := json.Marshal(body)
	if err != nil {
		return nil, fmt.Errorf("encode telegram %s request: %w", method, err)
	}

	for attempt := 0; ; attempt++ {
		result, err := c.callOnce(ctx, method, encoded)
		if err == nil {
			return result, nil
		}

		var apiErr *APIError
		// Only a 429 is retried, and only for a delay short enough to sit through: anything longer
		// is handed back so the caller can decide, because this process must stay responsive to the
		// update loop that carries the emergency stop.
		if !errors.As(err, &apiErr) || apiErr.RetryAfter <= 0 || apiErr.RetryAfter > maxRetryAfter || attempt >= rateLimitRetries {
			return nil, err
		}
		if !c.sleep(ctx, apiErr.RetryAfter) {
			return nil, err
		}
	}
}

func (c *Client) callOnce(ctx context.Context, method string, encoded []byte) (json.RawMessage, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, c.apiBase+"/bot"+c.token+"/"+method, bytes.NewReader(encoded))
	if err != nil {
		return nil, c.redact(fmt.Errorf("create telegram %s request: %w", method, err))
	}
	request.Header.Set("Content-Type", "application/json")

	response, err := c.http.Do(request)
	if err != nil {
		return nil, c.redact(fmt.Errorf("perform telegram %s request: %w", method, err))
	}
	defer response.Body.Close()

	body, err := io.ReadAll(io.LimitReader(response.Body, maxResponseBytes))
	if err != nil {
		return nil, c.redact(fmt.Errorf("read telegram %s response: %w", method, err))
	}

	var envelope struct {
		OK          bool            `json:"ok"`
		Result      json.RawMessage `json:"result"`
		Description string          `json:"description"`
		Parameters  struct {
			RetryAfter float64 `json:"retry_after"`
		} `json:"parameters"`
	}
	// The body is parsed before the status is judged: a 429 carries the wait in it, and an error
	// status carries the description that says what was actually wrong.
	decodeErr := json.Unmarshal(body, &envelope)

	if response.StatusCode == http.StatusTooManyRequests {
		return nil, &APIError{
			Method:      method,
			StatusCode:  response.StatusCode,
			Description: envelope.Description,
			RetryAfter:  retryAfterOf(envelope.Parameters.RetryAfter, response.Header.Get("Retry-After")),
		}
	}
	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		return nil, &APIError{Method: method, StatusCode: response.StatusCode, Description: envelope.Description}
	}
	if decodeErr != nil {
		return nil, fmt.Errorf("decode telegram %s response: %w", method, decodeErr)
	}
	if !envelope.OK {
		return nil, &APIError{Method: method, StatusCode: response.StatusCode, Description: envelope.Description}
	}

	return envelope.Result, nil
}

func retryAfterOf(fromBody float64, fromHeader string) time.Duration {
	if fromBody > 0 {
		return time.Duration(fromBody * float64(time.Second))
	}
	if seconds, err := strconv.Atoi(strings.TrimSpace(fromHeader)); err == nil && seconds > 0 {
		return time.Duration(seconds) * time.Second
	}
	return defaultRetryAfter
}

func (c *Client) GetUpdates(offset int64, timeoutSecs int) ([]Update, error) {
	return c.GetUpdatesContext(context.Background(), offset, timeoutSecs)
}

// GetUpdatesContext is the long poll the whole sidecar hangs off. It takes a context so shutdown
// does not have to wait out the poll it is already blocked in.
func (c *Client) GetUpdatesContext(ctx context.Context, offset int64, timeoutSecs int) ([]Update, error) {
	result, err := c.callContext(ctx, "getUpdates", map[string]any{
		"offset":          offset,
		"timeout":         timeoutSecs,
		"allowed_updates": []string{"message", "callback_query"},
	})
	if err != nil {
		return nil, err
	}

	var updates []Update
	if err := json.Unmarshal(result, &updates); err != nil {
		return nil, fmt.Errorf("decode telegram getUpdates result: %w", err)
	}
	return updates, nil
}

// GetFile resolves a file_id to a downloadable file_path via the Bot API getFile method.
func (c *Client) GetFile(fileID string) (string, error) {
	raw, err := c.call("getFile", map[string]any{"file_id": fileID})
	if err != nil {
		return "", err
	}
	var file File
	if err := json.Unmarshal(raw, &file); err != nil {
		return "", fmt.Errorf("parse getFile response: %w", err)
	}
	return file.FilePath, nil
}

// DownloadFile fetches the raw bytes for a file_path returned by GetFile. The download URL carries
// the bot token too, so its failures are redacted exactly like the API ones.
func (c *Client) DownloadFile(filePath string) ([]byte, error) {
	resp, err := c.http.Get(c.apiBase + "/file/bot" + c.token + "/" + filePath)
	if err != nil {
		return nil, c.redact(fmt.Errorf("download file: %w", err))
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("download file: status %d", resp.StatusCode)
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxDownloadBytes))
	if err != nil {
		return nil, c.redact(fmt.Errorf("download file: %w", err))
	}
	return data, nil
}

func (c *Client) SendMessage(to Destination, text string) error {
	request := to.body()
	request["text"] = text
	_, err := c.call("sendMessage", request)
	return err
}

// SendHTML sends a message with parse_mode=HTML (Telegram renders <b>/<i>/<code>/<pre>/<a>).
func (c *Client) SendHTML(to Destination, html string) error {
	request := to.body()
	request["text"] = html
	request["parse_mode"] = "HTML"
	_, err := c.call("sendMessage", request)
	return err
}

func (c *Client) SendMessageWithButtons(to Destination, text string, rows [][]Button) error {
	inlineKeyboard := make([][]map[string]string, len(rows))
	for rowIndex, row := range rows {
		inlineKeyboard[rowIndex] = make([]map[string]string, len(row))
		for buttonIndex, button := range row {
			inlineKeyboard[rowIndex][buttonIndex] = map[string]string{
				"text":          button.Text,
				"callback_data": button.CallbackData,
			}
		}
	}

	request := to.body()
	request["text"] = text
	request["reply_markup"] = map[string]any{"inline_keyboard": inlineKeyboard}
	_, err := c.call("sendMessage", request)
	return err
}

func (c *Client) AnswerCallbackQuery(callbackID, text string) error {
	_, err := c.call("answerCallbackQuery", map[string]any{
		"callback_query_id": callbackID,
		"text":              text,
	})
	return err
}

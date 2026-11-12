package telegram

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"mime"
	"mime/multipart"
	"net/http"
	"net/textproto"
	"strconv"
	"strings"
	"time"
)

// maxCaptionRunes is Telegram's limit on a media caption, in characters.
const maxCaptionRunes = 1024

// uploadTimeout bounds one whole upload. The client's own timeout is sized for a long poll and would
// cut a 20 MiB upload on a slow link; the caller's context still governs cancellation.
const uploadTimeout = 5 * time.Minute

// MediaKind is the kind of media being sent. The values match the contract's media kinds, but this
// package does not import the contract: it is the Bot API layer and stays ignorant of the kit.
type MediaKind string

const (
	MediaImage MediaKind = "image"
	MediaAudio MediaKind = "audio"
	MediaFile  MediaKind = "file"
)

// SendMediaParams describes one outgoing media message.
type SendMediaParams struct {
	To       Destination
	Filename string
	Mime     string
	Caption  string
	ReplyTo  int64
	Silent   bool
}

// MediaSent is what Telegram accepted, plus what could not ride along.
type MediaSent struct {
	MessageID int64
	// CaptionOverflow is the part of the caption past Telegram's 1024-character limit. It was NOT
	// sent: the caller decides whether to follow up with a text message carrying it.
	CaptionOverflow string
}

// SplitCaption cuts a caption at Telegram's limit, on a character boundary, and returns the
// remainder. A caller that wants to place the cut itself (at a word, say) can use it up front.
func SplitCaption(caption string) (head, overflow string) {
	if len(caption) <= maxCaptionRunes { // fewer bytes than the limit means fewer characters too
		return caption, ""
	}
	runes := []rune(caption)
	if len(runes) <= maxCaptionRunes {
		return caption, ""
	}
	return string(runes[:maxCaptionRunes]), string(runes[maxCaptionRunes:])
}

// mediaMethod picks the Bot API method and form field for a kind. An Ogg/Opus recording is a voice
// message; any other audio is a music-style audio file.
func mediaMethod(kind MediaKind, mimeType string) (method, field string, err error) {
	switch kind {
	case MediaImage:
		return "sendPhoto", "photo", nil
	case MediaFile:
		return "sendDocument", "document", nil
	case MediaAudio:
		if isOggOpus(mimeType) {
			return "sendVoice", "voice", nil
		}
		return "sendAudio", "audio", nil
	}
	return "", "", fmt.Errorf("unsupported media kind %q", kind)
}

// isOggOpus is true for audio/ogg without a codec parameter (Telegram voice notes are Ogg/Opus) or
// with codecs=opus; Ogg/Vorbis is not accepted by sendVoice.
func isOggOpus(mimeType string) bool {
	base, params, err := mime.ParseMediaType(mimeType)
	if err != nil || !strings.EqualFold(base, "audio/ogg") {
		return false
	}
	codecs := strings.ToLower(params["codecs"])
	return codecs == "" || strings.Contains(codecs, "opus")
}

// SendMedia uploads r and returns the id Telegram gave the message. It drops any caption overflow;
// use SendMediaEx to receive it.
func (c *Client) SendMedia(ctx context.Context, kind MediaKind, p SendMediaParams, r io.Reader) (int64, error) {
	sent, err := c.SendMediaEx(ctx, kind, p, r)
	return sent.MessageID, err
}

// SendMediaEx is SendMedia that also reports the caption overflow.
//
// The body is streamed through a pipe, so a 20 MiB file is never held in memory. A stream cannot be
// replayed, so a 429 is returned at once as an *APIError with RetryAfter and nothing is slept or
// retried here: the caller owns the decision, and owns the reader.
func (c *Client) SendMediaEx(ctx context.Context, kind MediaKind, p SendMediaParams, r io.Reader) (MediaSent, error) {
	method, field, err := mediaMethod(kind, p.Mime)
	if err != nil {
		return MediaSent{}, err
	}
	caption, overflow := SplitCaption(p.Caption)

	filename := p.Filename
	if filename == "" {
		filename = "file"
	}
	partHeader := textproto.MIMEHeader{}
	partHeader.Set("Content-Disposition", mime.FormatMediaType("form-data", map[string]string{"name": field, "filename": filename}))
	contentType := p.Mime
	if contentType == "" {
		contentType = "application/octet-stream"
	}
	partHeader.Set("Content-Type", contentType)

	pipeReader, pipeWriter := io.Pipe()
	form := multipart.NewWriter(pipeWriter)
	go func() {
		// The file goes last: Telegram has read every field by the time the long part starts, and a
		// reader that fails midway aborts the request instead of finishing a truncated upload.
		fail := func(err error) { _ = pipeWriter.CloseWithError(err) }
		fields := []struct{ name, value string }{{"chat_id", strconv.FormatInt(p.To.ChatID, 10)}}
		if p.To.ThreadID != 0 {
			fields = append(fields, struct{ name, value string }{"message_thread_id", strconv.FormatInt(p.To.ThreadID, 10)})
		}
		if caption != "" {
			fields = append(fields, struct{ name, value string }{"caption", caption})
		}
		if p.Silent {
			fields = append(fields, struct{ name, value string }{"disable_notification", "true"})
		}
		if p.ReplyTo != 0 {
			reply, _ := json.Marshal(map[string]any{"message_id": p.ReplyTo, "allow_sending_without_reply": true})
			fields = append(fields, struct{ name, value string }{"reply_parameters", string(reply)})
		}
		for _, f := range fields {
			if err := form.WriteField(f.name, f.value); err != nil {
				fail(err)
				return
			}
		}
		part, err := form.CreatePart(partHeader)
		if err != nil {
			fail(err)
			return
		}
		if _, err := io.Copy(part, r); err != nil {
			fail(err)
			return
		}
		if err := form.Close(); err != nil {
			fail(err)
			return
		}
		_ = pipeWriter.Close()
	}()
	// Closing the read side unblocks the writer goroutine whatever way this call ends.
	defer pipeReader.Close()

	request, err := http.NewRequestWithContext(ctx, http.MethodPost, c.apiBase+"/bot"+c.token+"/"+method, pipeReader)
	if err != nil {
		return MediaSent{}, c.redact(fmt.Errorf("create telegram %s request: %w", method, err))
	}
	request.Header.Set("Content-Type", form.FormDataContentType())

	upload := *c.http
	upload.Timeout = uploadTimeout
	response, err := upload.Do(request)
	if err != nil {
		return MediaSent{}, c.redact(fmt.Errorf("perform telegram %s request: %w", method, err))
	}
	defer response.Body.Close()

	body, err := io.ReadAll(io.LimitReader(response.Body, maxResponseBytes))
	if err != nil {
		return MediaSent{}, c.redact(fmt.Errorf("read telegram %s response: %w", method, err))
	}
	var envelope struct {
		OK          bool   `json:"ok"`
		Description string `json:"description"`
		Result      struct {
			MessageID int64 `json:"message_id"`
		} `json:"result"`
		Parameters struct {
			RetryAfter float64 `json:"retry_after"`
		} `json:"parameters"`
	}
	decodeErr := json.Unmarshal(body, &envelope)

	if response.StatusCode == http.StatusTooManyRequests {
		return MediaSent{}, &APIError{
			Method:      method,
			StatusCode:  response.StatusCode,
			Description: envelope.Description,
			RetryAfter:  retryAfterOf(envelope.Parameters.RetryAfter, response.Header.Get("Retry-After")),
		}
	}
	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices || (decodeErr == nil && !envelope.OK) {
		return MediaSent{}, &APIError{Method: method, StatusCode: response.StatusCode, Description: envelope.Description}
	}
	if decodeErr != nil {
		return MediaSent{}, fmt.Errorf("decode telegram %s response: %w", method, decodeErr)
	}
	return MediaSent{MessageID: envelope.Result.MessageID, CaptionOverflow: overflow}, nil
}

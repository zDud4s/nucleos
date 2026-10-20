package telegram

import (
	"bytes"
	"context"
	"errors"
	"io"
	"mime"
	"mime/multipart"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

type uploadSeen struct {
	method   string
	fields   map[string]string
	filePart string // form field name that carried the file
	filename string
	fileType string
	file     []byte
}

// newUploadAPI is a fake Bot API that parses the multipart body and answers with status/reply.
func newUploadAPI(t *testing.T, status int, reply string) (*uploadSeen, *Client, *atomic.Int32) {
	t.Helper()
	seen := &uploadSeen{fields: map[string]string{}}
	calls := &atomic.Int32{}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		seen.method = r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:]
		_, params, err := mime.ParseMediaType(r.Header.Get("Content-Type"))
		if err == nil {
			reader := multipart.NewReader(r.Body, params["boundary"])
			for {
				part, err := reader.NextPart()
				if err != nil {
					break
				}
				data, _ := io.ReadAll(part)
				if part.FileName() != "" {
					seen.filePart, seen.filename, seen.fileType, seen.file = part.FormName(), part.FileName(), part.Header.Get("Content-Type"), data
				} else {
					seen.fields[part.FormName()] = string(data)
				}
			}
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_, _ = w.Write([]byte(reply))
	}))
	t.Cleanup(server.Close)
	client := New("123:SECRET-TOKEN")
	client.apiBase = server.URL
	client.sleep = func(context.Context, time.Duration) bool { return true }
	return seen, client, calls
}

func TestSendMediaRoutesEachKind(t *testing.T) {
	cases := []struct {
		name, kind, mime, method, field string
	}{
		{"voice", "audio", "audio/ogg; codecs=opus", "sendVoice", "voice"},
		{"voice bare ogg", "audio", "audio/ogg", "sendVoice", "voice"},
		{"other audio", "audio", "audio/mpeg", "sendAudio", "audio"},
		{"image", "image", "image/png", "sendPhoto", "photo"},
		{"file", "file", "application/pdf", "sendDocument", "document"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			seen, client, _ := newUploadAPI(t, 200, `{"ok":true,"result":{"message_id":555}}`)
			payload := bytes.Repeat([]byte("\x00\x01binary"), 5000)
			id, err := client.SendMedia(context.Background(), MediaKind(tc.kind), SendMediaParams{
				To:       Destination{ChatID: 42, ThreadID: 9},
				Filename: "thing.bin",
				Mime:     tc.mime,
				Caption:  "hello",
				ReplyTo:  31,
				Silent:   true,
			}, bytes.NewReader(payload))
			if err != nil || id != 555 {
				t.Fatalf("id = %d err = %v, want 555 nil", id, err)
			}
			if seen.method != tc.method || seen.filePart != tc.field {
				t.Errorf("method/field = %s/%s, want %s/%s", seen.method, seen.filePart, tc.method, tc.field)
			}
			if !bytes.Equal(seen.file, payload) || seen.filename != "thing.bin" {
				t.Errorf("file = %q (%d bytes), want thing.bin with the payload", seen.filename, len(seen.file))
			}
			for k, want := range map[string]string{"chat_id": "42", "message_thread_id": "9", "caption": "hello", "disable_notification": "true"} {
				if seen.fields[k] != want {
					t.Errorf("field %s = %q, want %q", k, seen.fields[k], want)
				}
			}
			if rp := seen.fields["reply_parameters"]; !strings.Contains(rp, `"message_id":31`) || !strings.Contains(rp, `"allow_sending_without_reply":true`) {
				t.Errorf("reply_parameters = %q", rp)
			}
		})
	}
}

func TestSendMediaOmitsOptionalFields(t *testing.T) {
	seen, client, _ := newUploadAPI(t, 200, `{"ok":true,"result":{"message_id":1}}`)
	_, err := client.SendMedia(context.Background(), "file", SendMediaParams{To: Destination{ChatID: 7}, Filename: "a.txt"}, strings.NewReader("x"))
	if err != nil {
		t.Fatal(err)
	}
	for _, k := range []string{"message_thread_id", "caption", "reply_parameters", "disable_notification"} {
		if _, ok := seen.fields[k]; ok {
			t.Errorf("field %s present, want omitted", k)
		}
	}
}

func TestSendMediaUnknownKind(t *testing.T) {
	_, client, calls := newUploadAPI(t, 200, `{"ok":true,"result":{"message_id":1}}`)
	if _, err := client.SendMedia(context.Background(), "video", SendMediaParams{To: Destination{ChatID: 1}}, strings.NewReader("x")); err == nil {
		t.Fatal("want an error for an unknown kind")
	}
	if calls.Load() != 0 {
		t.Error("an unknown kind must not reach the network")
	}
}

func TestSendMediaCaptionTrimmedWithOverflowSurfaced(t *testing.T) {
	seen, client, _ := newUploadAPI(t, 200, `{"ok":true,"result":{"message_id":9}}`)
	// Multi-byte runes: the cut is on characters, never in the middle of one.
	caption := strings.Repeat("é", 1024) + "tail"
	sent, err := client.SendMediaEx(context.Background(), "image", SendMediaParams{To: Destination{ChatID: 1}, Filename: "p.png", Mime: "image/png", Caption: caption}, strings.NewReader("png"))
	if err != nil {
		t.Fatal(err)
	}
	if got := seen.fields["caption"]; got != strings.Repeat("é", 1024) {
		t.Errorf("sent caption has %d runes, want 1024", len([]rune(got)))
	}
	if sent.CaptionOverflow != "tail" || sent.MessageID != 9 {
		t.Errorf("sent = %+v, want overflow %q and id 9", sent, "tail")
	}
}

func TestSendMediaShortCaptionHasNoOverflow(t *testing.T) {
	_, client, _ := newUploadAPI(t, 200, `{"ok":true,"result":{"message_id":9}}`)
	sent, err := client.SendMediaEx(context.Background(), "file", SendMediaParams{To: Destination{ChatID: 1}, Filename: "a", Caption: "short"}, strings.NewReader("x"))
	if err != nil || sent.CaptionOverflow != "" {
		t.Fatalf("sent = %+v err = %v, want no overflow", sent, err)
	}
}

func TestSplitCaption(t *testing.T) {
	head, rest := SplitCaption(strings.Repeat("a", 1030))
	if len(head) != 1024 || rest != strings.Repeat("a", 6) {
		t.Errorf("head %d rest %q", len(head), rest)
	}
	if head, rest := SplitCaption("ok"); head != "ok" || rest != "" {
		t.Errorf("short caption changed: %q %q", head, rest)
	}
}

func TestSendMedia429ReturnsAtOnceWithoutSleeping(t *testing.T) {
	_, client, calls := newUploadAPI(t, 429, `{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":2}}`)
	slept := false
	client.sleep = func(context.Context, time.Duration) bool { slept = true; return true }
	_, err := client.SendMedia(context.Background(), "file", SendMediaParams{To: Destination{ChatID: 1}, Filename: "a"}, strings.NewReader("x"))
	var apiErr *APIError
	if !errors.As(err, &apiErr) || apiErr.RetryAfter != 2*time.Second || !IsRateLimited(err) {
		t.Fatalf("err = %v, want APIError with RetryAfter 2s", err)
	}
	if slept || calls.Load() != 1 {
		t.Errorf("slept = %v calls = %d, want no sleep and exactly one call", slept, calls.Load())
	}
}

func TestSendMediaAPIErrorCarriesDescription(t *testing.T) {
	_, client, _ := newUploadAPI(t, 400, `{"ok":false,"description":"Bad Request: file too big"}`)
	_, err := client.SendMedia(context.Background(), "file", SendMediaParams{To: Destination{ChatID: 1}, Filename: "a"}, strings.NewReader("x"))
	var apiErr *APIError
	if !errors.As(err, &apiErr) || apiErr.StatusCode != 400 || apiErr.Description != "Bad Request: file too big" || IsTransport(err) {
		t.Fatalf("err = %v", err)
	}
}

func TestSendMediaTransportErrorNeverContainsTheToken(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
	url := server.URL
	server.Close()
	client := New("123:SECRET-TOKEN")
	client.apiBase = url
	_, err := client.SendMedia(context.Background(), "file", SendMediaParams{To: Destination{ChatID: 1}, Filename: "a"}, strings.NewReader("x"))
	if err == nil {
		t.Fatal("want a transport error")
	}
	if strings.Contains(err.Error(), "SECRET-TOKEN") {
		t.Errorf("error leaks the token: %v", err)
	}
	if !IsTransport(err) {
		t.Errorf("err = %v, want a transport error", err)
	}
}

func TestSendMediaBadURLErrorNeverContainsTheToken(t *testing.T) {
	client := New("123:SECRET-TOKEN")
	client.apiBase = "http://bad host\x7f"
	_, err := client.SendMedia(context.Background(), "file", SendMediaParams{To: Destination{ChatID: 1}, Filename: "a"}, strings.NewReader("x"))
	if err == nil || strings.Contains(err.Error(), "SECRET-TOKEN") {
		t.Fatalf("err = %v, want an error without the token", err)
	}
}

type failingReader struct{ sent bool }

func (f *failingReader) Read(p []byte) (int, error) {
	if !f.sent {
		f.sent = true
		return copy(p, bytes.Repeat([]byte("a"), 100)), nil
	}
	return 0, errors.New("disk went away")
}

func TestSendMediaReaderFailingMidwayIsTransient(t *testing.T) {
	_, client, _ := newUploadAPI(t, 200, `{"ok":true,"result":{"message_id":1}}`)
	_, err := client.SendMedia(context.Background(), "file", SendMediaParams{To: Destination{ChatID: 1}, Filename: "a"}, &failingReader{})
	if err == nil {
		t.Fatal("want an error")
	}
	if !IsTransport(err) {
		t.Errorf("err = %v, want a transport-classified (transient) error", err)
	}
	if strings.Contains(err.Error(), "SECRET-TOKEN") {
		t.Errorf("error leaks the token: %v", err)
	}
}

package adapter

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	"nucleoschannelkit/contract"

	"nucleostelegram/telegram"
)

const captureSecret = "the-owner-private-answer-4711"

type fakeCaptureAPI struct {
	mu       sync.Mutex
	jobs     []int64
	texts    []string
	released bool
	err      error
}

func (f *fakeCaptureAPI) Answer(_ context.Context, jobID int64, text string) (CaptureAnswer, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.jobs = append(f.jobs, jobID)
	f.texts = append(f.texts, text)
	if f.err != nil {
		return CaptureAnswer{}, f.err
	}
	return CaptureAnswer{NoteID: 77, Released: f.released}, nil
}

type fakeCaptureBot struct{ sent []telegram.SendParams }

func (b *fakeCaptureBot) SendHTMLEx(_ context.Context, p telegram.SendParams) (int64, error) {
	b.sent = append(b.sent, p)
	return 1, nil
}

// capReply is a reply from the owner (4242) to a message of the bot carrying botText.
func capReply(text, botText string, botIsBot bool) telegram.Update {
	u := privateText(10, 20, text)
	u.Message.ReplyToMessage = &telegram.Message{
		MessageID: 19,
		Chat:      telegram.Chat{ID: 4242, Type: "private"},
		From:      &telegram.User{ID: 999, IsBot: botIsBot},
		Text:      botText,
	}
	return u
}

func runCapture(t *testing.T, api *fakeCaptureAPI, u telegram.Update) (*fakePoster, *fakeCaptureBot, bool) {
	t.Helper()
	bot := &fakeCaptureBot{}
	sink := &fakePoster{}
	adv, err := handleUpdate(context.Background(), fakeAttacher{}, sink, &Capture{API: api, Bot: bot}, u)
	if err != nil {
		t.Fatal(err)
	}
	return sink, bot, adv
}

func TestCaptureReplyAnswersWithoutATurn(t *testing.T) {
	api := &fakeCaptureAPI{released: true}
	sink, bot, adv := runCapture(t, api, capReply("a resposta", "Que decisão ficou?\n#cap42", true))
	if !adv || len(sink.events) != 0 {
		t.Fatalf("advance=%v events=%d, want advance and no event", adv, len(sink.events))
	}
	if len(api.jobs) != 1 || api.jobs[0] != 42 || api.texts[0] != "a resposta" {
		t.Fatalf("answer calls = %v %v", api.jobs, api.texts)
	}
	if len(bot.sent) != 1 || bot.sent[0].Text != "anotado (#77), ligado ao job #42" || bot.sent[0].ReplyToMessageID != 20 {
		t.Fatalf("sent = %+v", bot.sent)
	}
}

func TestCaptureReplyLateAnswer(t *testing.T) {
	api := &fakeCaptureAPI{released: false}
	_, bot, _ := runCapture(t, api, capReply("tarde", "#cap5", true))
	if len(bot.sent) != 1 || bot.sent[0].Text != "anotado; o destilador já avançou neste job" {
		t.Fatalf("sent = %+v", bot.sent)
	}
}

func TestReplyWithoutMarkOrFromNonBotIsAnOrdinaryTurn(t *testing.T) {
	for name, u := range map[string]telegram.Update{
		"no mark":     capReply("olá", "just a message", true),
		"not a bot":   capReply("olá", "#cap42", false),
		"not a reply": privateText(10, 20, "#cap42"),
		"bad job id":  capReply("olá", "#cap0", true),
		"mark in own": capReply("#cap42", "nothing", true),
		"no from": func() telegram.Update {
			u := capReply("x", "#cap1", true)
			u.Message.ReplyToMessage.From = nil
			return u
		}(),
		"other sender": func() telegram.Update { u := capReply("x", "#cap1", true); u.Message.From = nil; return u }(),
	} {
		api := &fakeCaptureAPI{released: true}
		sink, bot, adv := runCapture(t, api, u)
		if name == "other sender" {
			// no usable sender: dropped like any update, never a capture answer
			if len(api.jobs) != 0 || len(sink.events) != 0 || !adv {
				t.Fatalf("%s: jobs=%v events=%d adv=%v", name, api.jobs, len(sink.events), adv)
			}
			continue
		}
		if len(api.jobs) != 0 || len(bot.sent) != 0 || len(sink.events) != 1 {
			t.Fatalf("%s: jobs=%v sent=%d events=%d, want one normal turn", name, api.jobs, len(bot.sent), len(sink.events))
		}
	}
}

func TestCaptureDictatedReplyIsRefusedAndNotTranscribed(t *testing.T) {
	api := &fakeCaptureAPI{released: true}
	u := capReply("", "#cap42", true)
	u.Message.Voice = &telegram.Voice{FileID: "v", Duration: 2, FileSize: 3}
	att := &countingAttacher{}
	bot := &fakeCaptureBot{}
	sink := &fakePoster{}
	adv, err := handleUpdate(context.Background(), att, sink, &Capture{API: api, Bot: bot}, u)
	if err != nil || !adv {
		t.Fatalf("adv=%v err=%v", adv, err)
	}
	if att.n != 0 || len(sink.events) != 0 || len(api.jobs) != 0 {
		t.Fatalf("attach=%d events=%d answers=%d, want none", att.n, len(sink.events), len(api.jobs))
	}
	if len(bot.sent) != 1 || bot.sent[0].Text != captureRefusal {
		t.Fatalf("sent = %+v", bot.sent)
	}
}

func TestCaptureReplyWithPhotoOrDocumentIsRefused(t *testing.T) {
	for _, mut := range []func(*telegram.Message){
		func(m *telegram.Message) { m.Photo = []telegram.PhotoSize{{FileID: "p"}} },
		func(m *telegram.Message) { m.Document = &telegram.Document{FileID: "d"} },
	} {
		u := capReply("", "#cap42", true)
		u.Message.Caption = "legenda"
		mut(u.Message)
		api := &fakeCaptureAPI{released: true}
		_, bot, _ := runCapture(t, api, u)
		if len(api.jobs) != 0 || len(bot.sent) != 1 || bot.sent[0].Text != captureRefusal {
			t.Fatalf("jobs=%v sent=%+v", api.jobs, bot.sent)
		}
	}
}

func TestCaptureTransientFailureRetriesAndRejectionReplies(t *testing.T) {
	api := &fakeCaptureAPI{err: errors.New("boom")}
	_, bot, adv := runCapture(t, api, capReply("x", "#cap1", true))
	if adv || len(bot.sent) != 0 {
		t.Fatalf("transient: adv=%v sent=%d, want retry and silence", adv, len(bot.sent))
	}
	api = &fakeCaptureAPI{err: ErrCaptureRejected}
	_, bot, adv = runCapture(t, api, capReply("x", "#cap1", true))
	if !adv || len(bot.sent) != 1 || bot.sent[0].Text != captureNotSaved {
		t.Fatalf("rejected: adv=%v sent=%+v", adv, bot.sent)
	}
}

func TestCaptureReplyTextNeverLogged(t *testing.T) {
	for _, api := range []*fakeCaptureAPI{
		{released: true}, {released: false}, {err: errors.New("boom")}, {err: ErrCaptureRejected},
	} {
		logs := captureLogs(t)
		runCapture(t, api, capReply(captureSecret, "#cap42", true))
		if strings.Contains(logs.String(), captureSecret) {
			t.Fatalf("reply text leaked into logs:\n%s", logs.String())
		}
	}
}

func TestCaptureHTTPPostsTextAndOrigin(t *testing.T) {
	var got map[string]string
	var path, auth string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		path, auth = r.URL.Path, r.Header.Get("Authorization")
		_ = json.NewDecoder(r.Body).Decode(&got)
		w.WriteHeader(http.StatusCreated)
		_, _ = w.Write([]byte(`{"note_id":9,"released":true}`))
	}))
	defer srv.Close()
	ans, err := CaptureHTTP{BaseURL: srv.URL + "/", Token: "tok"}.Answer(context.Background(), 42, "olá")
	if err != nil || ans.NoteID != 9 || !ans.Released {
		t.Fatalf("ans=%+v err=%v", ans, err)
	}
	if path != "/capture-requests/42/answer" || auth != "Bearer tok" || got["text"] != "olá" || got["origin"] != "telegram" {
		t.Fatalf("path=%q auth=%q body=%v", path, auth, got)
	}
}

func TestCaptureHTTPStatuses(t *testing.T) {
	for status, rejected := range map[int]bool{400: true, 404: true, 500: false, 401: false} {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(status) }))
		_, err := CaptureHTTP{BaseURL: srv.URL}.Answer(context.Background(), 1, "x")
		srv.Close()
		if err == nil || errors.Is(err, ErrCaptureRejected) != rejected {
			t.Fatalf("status %d: err=%v want rejected=%v", status, err, rejected)
		}
	}
}

type countingAttacher struct{ n int }

func (a *countingAttacher) Attach(context.Context, *contract.Event, *telegram.Message) error {
	a.n++
	return nil
}

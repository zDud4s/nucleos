package adapter

import (
	"bytes"
	"context"
	"errors"
	"log"
	"log/slog"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"

	"nucleostelegram/telegram"
)

// Part C of the second-brain design (S10, S11): /note is a core command. The sidecar must carry the
// text through untouched (S10) and must never write a note's text to a log (S11).

const noteSecret = "buy-milk-and-the-secret-plan"

// lockedBuf is a log sink safe for the kit's goroutines.
type lockedBuf struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func (l *lockedBuf) Write(p []byte) (int, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.b.Write(p)
}

func (l *lockedBuf) String() string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.b.String()
}

// captureLogs redirects the std logger and the slog default for the test.
func captureLogs(t *testing.T) *lockedBuf {
	t.Helper()
	buf := &lockedBuf{}
	prevW, prevSlog := log.Writer(), slog.Default()
	log.SetOutput(buf)
	slog.SetDefault(slog.New(slog.NewTextHandler(buf, &slog.HandlerOptions{Level: slog.LevelDebug})))
	t.Cleanup(func() {
		log.SetOutput(prevW)
		slog.SetDefault(prevSlog)
	})
	return buf
}

// S10: /note text, with or without the @Bot suffix, is an ordinary inbound text, verbatim.
func TestNoteCommandForwardedVerbatimAsText(t *testing.T) {
	for _, text := range []string{"/note buy milk", "/note@Bot buy milk", "/note"} {
		ev, needsMedia, ok := Inbound(privateText(1, 2, text))
		if !ok || needsMedia {
			t.Fatalf("%q: ok=%v needsMedia=%v", text, ok, needsMedia)
		}
		parts := ev.Message.Parts
		if len(parts) != 1 || *parts[0].Type != contract.PartText || *parts[0].Text != text {
			t.Fatalf("%q: parts = %+v, want one verbatim text part", text, parts)
		}
		if *ev.Type != "message" {
			t.Fatalf("%q: type = %q, want message", text, *ev.Type)
		}
		assertDecodes(t, ev)
	}
}

// S10 end to end: the core receives the /note update as a message event with the exact text.
func TestE2ENoteCommandReachesCoreVerbatim(t *testing.T) {
	g := newE2ERig(t)
	g.start(nil)
	g.bot.push(privateText(10, 1, "/note buy milk"), privateText(11, 2, "/note@Bot buy milk"))

	evs := g.events(2)
	for i, want := range []string{"/note buy milk", "/note@Bot buy milk"} {
		parts := evs[i].Message.Parts
		if len(parts) != 1 || *parts[0].Type != contract.PartText || *parts[0].Text != want {
			t.Fatalf("event %d parts = %+v, want text %q", i, parts, want)
		}
	}
}

// S10: a voice message reaches the core as an audio part (never as text), so the core can refuse a
// heard command; its caption stays an ordinary text part.
func TestNoteVoiceIsForwardedAsAudioNotText(t *testing.T) {
	msg := mediaMsg()
	msg.Voice = &telegram.Voice{FileID: "v", Duration: 2, FileSize: 3}
	ev := baseEvent(t, msg)
	bot := &fakeBot{files: map[string][]byte{"v": []byte("ogg")}}
	if err := AttachMedia(context.Background(), &ev, msg, bot, &fakeUploader{}); err != nil {
		t.Fatal(err)
	}
	if len(ev.Message.Parts) != 1 || *ev.Message.Parts[0].Type != contract.PartAudio {
		t.Fatalf("parts = %+v, want a single audio part", ev.Message.Parts)
	}
	if ev.Message.Parts[0].Text != nil {
		t.Fatalf("voice part carries text %q", *ev.Message.Parts[0].Text)
	}
}

// S11 success path: the note travels to the core and nothing of it reaches a log.
func TestNoteTextNeverLoggedOnSuccess(t *testing.T) {
	logs := captureLogs(t)
	g := newE2ERig(t)
	g.start(nil)
	g.bot.push(privateText(10, 1, "/note "+noteSecret))
	g.events(1)
	eventually(t, "offset to advance", func() bool { return g.savedOffset() == 11 })
	g.stop()
	if strings.Contains(logs.String(), noteSecret) {
		t.Fatalf("note text leaked into logs:\n%s", logs.String())
	}
}

// S11 failure path 1: the core answers 500, the event is re-posted; no log carries the text.
func TestNoteTextNeverLoggedWhenCoreFails(t *testing.T) {
	logs := captureLogs(t)
	g := newE2ERig(t)
	g.core.FailNext(fakecore.RouteEvents, 1, http.StatusInternalServerError, `{"error":"boom"}`)
	g.start(nil)
	g.bot.push(privateText(10, 1, "/note "+noteSecret))
	g.waitCalls(fakecore.RouteEvents, 1)
	g.pending(2)
	g.clk.Advance(time.Minute)
	g.events(1)
	g.stop()
	if strings.Contains(logs.String(), noteSecret) {
		t.Fatalf("note text leaked into logs:\n%s", logs.String())
	}
}

// S11 failure path 2: a reply that echoes the note fails to send; the error log names refs and the
// error, never the reply text.
func TestNoteTextNeverLoggedWhenSendFails(t *testing.T) {
	logs := captureLogs(t)
	s := contract.Send{ID: 9, Conversation: "4242", Parts: []contract.OutPart{
		{Type: contract.OutText, Text: "saved: " + noteSecret},
		{Type: contract.OutText, Text: "again: " + noteSecret},
	}}
	bot := &fakeSendBot{errs: map[int]error{1: apiErr(502, "Bad Gateway")}}
	_, err := New(bot, nil).Execute(context.Background(), s)
	if err == nil {
		t.Fatal("want the send to fail")
	}
	if errors.Unwrap(err) != nil && strings.Contains(err.Error(), noteSecret) {
		t.Fatalf("error carries the text: %v", err)
	}
	if strings.Contains(logs.String(), noteSecret) {
		t.Fatalf("reply text leaked into logs:\n%s", logs.String())
	}
}

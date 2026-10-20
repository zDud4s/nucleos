package adapter

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"reflect"
	"strings"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/kit"

	"nucleostelegram/telegram"
)

// fakeBot stands in for the Bot API: files maps file_id -> bytes; every file_id resolves to the path
// "path/<file_id>" unless getErr is set.
type fakeBot struct {
	files       map[string][]byte
	getErr      error
	downloadErr error
	downloaded  []string
}

func (b *fakeBot) GetFile(fileID string) (string, error) {
	if b.getErr != nil {
		return "", b.getErr
	}
	return "path/" + fileID, nil
}

func (b *fakeBot) DownloadFile(filePath string) ([]byte, error) {
	b.downloaded = append(b.downloaded, filePath)
	if b.downloadErr != nil {
		return nil, b.downloadErr
	}
	return b.files[strings.TrimPrefix(filePath, "path/")], nil
}

type upload struct {
	mime string
	data []byte
}

// fakeUploader hands out m_<n> ids starting at start and records what it was given.
type fakeUploader struct {
	next    int
	err     error
	uploads []upload
}

func (u *fakeUploader) Upload(_ context.Context, mime string, r io.Reader) (string, error) {
	data, _ := io.ReadAll(r)
	u.uploads = append(u.uploads, upload{mime, data})
	if u.err != nil {
		return "", u.err
	}
	u.next++
	return fmt.Sprintf("m_%d", u.next), nil
}

func baseEvent(t *testing.T, msg *telegram.Message) contract.Event {
	t.Helper()
	ev, _, ok := Inbound(telegram.Update{UpdateID: 1, Message: msg})
	if !ok {
		t.Fatal("Inbound dropped the message")
	}
	return ev
}

func mediaMsg() *telegram.Message {
	return &telegram.Message{
		MessageID: 5,
		Chat:      telegram.Chat{ID: 4242, Type: "private"},
		From:      &telegram.User{ID: 4242},
	}
}

// assertDecodes proves the event survives the strict decoder the daemon uses.
func assertDecodes(t *testing.T, ev contract.Event) {
	t.Helper()
	b, err := json.Marshal(ev)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := contract.DecodeStrict[contract.Event](b); err != nil {
		t.Fatalf("DecodeStrict: %v\n%s", err, b)
	}
}

func str(s string) *string { return &s }

func TestMediaInVoice(t *testing.T) {
	msg := mediaMsg()
	msg.Voice = &telegram.Voice{FileID: "v", Duration: 4, FileSize: 3}
	ev := baseEvent(t, msg)
	bot := &fakeBot{files: map[string][]byte{"v": []byte("ogg")}}
	up := &fakeUploader{}
	if err := AttachMedia(context.Background(), &ev, msg, bot, up); err != nil {
		t.Fatal(err)
	}
	parts := ev.Message.Parts
	if len(parts) != 1 {
		t.Fatalf("parts = %+v", parts)
	}
	p := parts[0]
	if *p.Type != contract.PartAudio || *p.Media != "m_1" || *p.Mime != "audio/ogg" || *p.DurationMS != 4000 {
		t.Fatalf("part = %+v", p)
	}
	if len(up.uploads) != 1 || up.uploads[0].mime != "audio/ogg" || string(up.uploads[0].data) != "ogg" {
		t.Fatalf("uploads = %+v", up.uploads)
	}
	assertDecodes(t, ev)
}

func TestMediaInVoiceKeepsDeclaredMime(t *testing.T) {
	msg := mediaMsg()
	msg.Voice = &telegram.Voice{FileID: "v", MimeType: "audio/opus"}
	ev := baseEvent(t, msg)
	up := &fakeUploader{}
	if err := AttachMedia(context.Background(), &ev, msg, &fakeBot{files: map[string][]byte{"v": {1}}}, up); err != nil {
		t.Fatal(err)
	}
	if *ev.Message.Parts[0].Mime != "audio/opus" || up.uploads[0].mime != "audio/opus" {
		t.Fatalf("part = %+v uploads = %+v", ev.Message.Parts[0], up.uploads)
	}
}

func TestMediaInAudioTrack(t *testing.T) {
	msg := mediaMsg()
	msg.Audio = &telegram.Audio{FileID: "a", Duration: 200, MimeType: "audio/mpeg"}
	ev := baseEvent(t, msg)
	up := &fakeUploader{}
	if err := AttachMedia(context.Background(), &ev, msg, &fakeBot{files: map[string][]byte{"a": {1}}}, up); err != nil {
		t.Fatal(err)
	}
	p := ev.Message.Parts[0]
	if *p.Type != contract.PartAudio || *p.Mime != "audio/mpeg" || *p.DurationMS != 200000 {
		t.Fatalf("part = %+v", p)
	}
	assertDecodes(t, ev)
}

func TestMediaInPhotoPicksLargest(t *testing.T) {
	msg := mediaMsg()
	msg.Photo = []telegram.PhotoSize{
		{FileID: "small", Width: 90, Height: 60},
		{FileID: "big", Width: 1280, Height: 720},
		{FileID: "mid", Width: 320, Height: 200},
	}
	ev := baseEvent(t, msg)
	bot := &fakeBot{files: map[string][]byte{"big": []byte("jpg")}}
	up := &fakeUploader{}
	if err := AttachMedia(context.Background(), &ev, msg, bot, up); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(bot.downloaded, []string{"path/big"}) {
		t.Fatalf("downloaded = %v", bot.downloaded)
	}
	p := ev.Message.Parts[0]
	if *p.Type != contract.PartImage || *p.Media != "m_1" {
		t.Fatalf("part = %+v", p)
	}
	if up.uploads[0].mime != "image/jpeg" {
		t.Fatalf("upload mime = %q", up.uploads[0].mime)
	}
	assertDecodes(t, ev)
}

func TestMediaInDocumentStripsMimeParameters(t *testing.T) {
	msg := mediaMsg()
	msg.Document = &telegram.Document{FileID: "d", FileName: "x.pdf", MimeType: "Application/PDF; charset=binary"}
	ev := baseEvent(t, msg)
	up := &fakeUploader{}
	if err := AttachMedia(context.Background(), &ev, msg, &fakeBot{files: map[string][]byte{"d": {1}}}, up); err != nil {
		t.Fatal(err)
	}
	p := ev.Message.Parts[0]
	if *p.Type != contract.PartFile || *p.Name != "x.pdf" || *p.Mime != "application/pdf" {
		t.Fatalf("part = %+v", p)
	}
	if up.uploads[0].mime != "application/pdf" {
		t.Fatalf("upload mime = %q", up.uploads[0].mime)
	}
	assertDecodes(t, ev)
}

func TestMediaInDocumentWithoutNameGetsFallback(t *testing.T) {
	msg := mediaMsg()
	msg.Document = &telegram.Document{FileID: "d"}
	ev := baseEvent(t, msg)
	up := &fakeUploader{}
	if err := AttachMedia(context.Background(), &ev, msg, &fakeBot{files: map[string][]byte{"d": {1}}}, up); err != nil {
		t.Fatal(err)
	}
	p := ev.Message.Parts[0]
	if p.Name == nil || *p.Name != "file0" {
		t.Fatalf("part = %+v, want name file0", p)
	}
	if p.Mime != nil {
		t.Fatalf("an undeclared mime must stay off the part, got %q", *p.Mime)
	}
	if up.uploads[0].mime != "application/octet-stream" {
		t.Fatalf("upload mime = %q", up.uploads[0].mime)
	}
	assertDecodes(t, ev)
}

func TestMediaInCaptionLandsOnThePart(t *testing.T) {
	msg := mediaMsg()
	msg.Photo = []telegram.PhotoSize{{FileID: "p"}}
	msg.Caption = "o ecrã de erro"
	ev := baseEvent(t, msg)
	if err := AttachMedia(context.Background(), &ev, msg, &fakeBot{files: map[string][]byte{"p": {1}}}, &fakeUploader{}); err != nil {
		t.Fatal(err)
	}
	var img *contract.InPart
	for i := range ev.Message.Parts {
		if *ev.Message.Parts[i].Type == contract.PartImage {
			img = &ev.Message.Parts[i]
		}
	}
	if img == nil || img.Caption == nil || *img.Caption != "o ecrã de erro" {
		t.Fatalf("parts = %+v", ev.Message.Parts)
	}
	assertDecodes(t, ev)
}

// countText returns how many text parts carry the given text.
func countText(ev contract.Event, text string) int {
	n := 0
	for _, p := range ev.Message.Parts {
		if *p.Type == contract.PartText && *p.Text == text {
			n++
		}
	}
	return n
}

// The contract gives image and file a caption of their own, so a captioned photo carries it there and
// nowhere else: a second, text copy would show the model the same words twice.
func TestMediaInCaptionIsNotRepeatedAsText(t *testing.T) {
	msg := mediaMsg()
	msg.Document = &telegram.Document{FileID: "d", FileName: "x.pdf"}
	msg.Photo = []telegram.PhotoSize{{FileID: "p"}}
	msg.Caption = "o relatório"
	ev := baseEvent(t, msg)
	files := map[string][]byte{"p": {1}, "d": {2}}
	if err := AttachMedia(context.Background(), &ev, msg, &fakeBot{files: files}, &fakeUploader{}); err != nil {
		t.Fatal(err)
	}
	if n := countText(ev, "o relatório"); n != 0 {
		t.Fatalf("caption repeated as %d text part(s): %+v", n, ev.Message.Parts)
	}
	for _, p := range ev.Message.Parts {
		if (*p.Type == contract.PartImage || *p.Type == contract.PartFile) && (p.Caption == nil || *p.Caption != "o relatório") {
			t.Fatalf("part lost its caption: %+v", p)
		}
	}
	assertDecodes(t, ev)
}

// An audio part has no caption field, so the caption of a voice note is its one text part.
func TestMediaInVoiceCaptionIsTheTextPartOnly(t *testing.T) {
	msg := mediaMsg()
	msg.Voice = &telegram.Voice{FileID: "v", Duration: 2}
	msg.Caption = "listen"
	ev := baseEvent(t, msg)
	if err := AttachMedia(context.Background(), &ev, msg, &fakeBot{files: map[string][]byte{"v": {1}}}, &fakeUploader{}); err != nil {
		t.Fatal(err)
	}
	if n := countText(ev, "listen"); n != 1 {
		t.Fatalf("want one text part, got %d: %+v", n, ev.Message.Parts)
	}
	for _, p := range ev.Message.Parts {
		if *p.Type == contract.PartAudio && p.Caption != nil {
			t.Fatalf("audio part carries a caption: %+v", p)
		}
	}
	assertDecodes(t, ev)
}

// If the captioned photo cannot be carried, the caption (the user's instruction) must not vanish with it.
func TestMediaInCaptionSurvivesAFailedPhoto(t *testing.T) {
	msg := mediaMsg()
	msg.Photo = []telegram.PhotoSize{{FileID: "p"}}
	msg.Caption = "o ecrã de erro"
	ev := baseEvent(t, msg)
	up := &fakeUploader{err: fmt.Errorf("wrapped: %w", kit.ErrMediaTooLarge)}
	if err := AttachMedia(context.Background(), &ev, msg, &fakeBot{files: map[string][]byte{"p": {1}}}, up); err != nil {
		t.Fatal(err)
	}
	if n := countText(ev, "o ecrã de erro"); n != 1 {
		t.Fatalf("want the caption as one text part, got %d: %+v", n, ev.Message.Parts)
	}
	assertUnsupported(t, ev, "photo")
}

func TestMediaInTooLargeUploadBecomesUnsupported(t *testing.T) {
	msg := mediaMsg()
	msg.Document = &telegram.Document{FileID: "d", FileName: "big.bin"}
	ev := baseEvent(t, msg)
	up := &fakeUploader{err: fmt.Errorf("wrapped: %w", kit.ErrMediaTooLarge)}
	if err := AttachMedia(context.Background(), &ev, msg, &fakeBot{files: map[string][]byte{"d": {1}}}, up); err != nil {
		t.Fatalf("413 must not fail the event: %v", err)
	}
	assertUnsupported(t, ev, "document")
}

func TestMediaInTransientUploadErrorPropagates(t *testing.T) {
	msg := mediaMsg()
	msg.Voice = &telegram.Voice{FileID: "v"}
	ev := baseEvent(t, msg)
	boom := errors.New("connection reset")
	up := &fakeUploader{err: boom}
	err := AttachMedia(context.Background(), &ev, msg, &fakeBot{files: map[string][]byte{"v": {1}}}, up)
	if !errors.Is(err, boom) {
		t.Fatalf("err = %v, want the transient error", err)
	}
}

func TestMediaInFailedDownloadBecomesUnsupported(t *testing.T) {
	cases := map[string]struct {
		bot   *fakeBot
		label string
		set   func(m *telegram.Message)
	}{
		"getFile":  {&fakeBot{getErr: errors.New("nope")}, "voice", func(m *telegram.Message) { m.Voice = &telegram.Voice{FileID: "x"} }},
		"download": {&fakeBot{downloadErr: errors.New("nope")}, "photo", func(m *telegram.Message) { m.Photo = []telegram.PhotoSize{{FileID: "x"}} }},
		"audio":    {&fakeBot{downloadErr: errors.New("nope")}, "audio", func(m *telegram.Message) { m.Audio = &telegram.Audio{FileID: "x"} }},
	}
	for name, c := range cases {
		msg := mediaMsg()
		c.set(msg)
		ev := baseEvent(t, msg)
		up := &fakeUploader{}
		if err := AttachMedia(context.Background(), &ev, msg, c.bot, up); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if len(up.uploads) != 0 {
			t.Fatalf("%s: uploaded %d times", name, len(up.uploads))
		}
		assertUnsupported(t, ev, c.label)
	}
}

func TestMediaInOversizeIsRefusedWithoutDownloading(t *testing.T) {
	msg := mediaMsg()
	msg.Document = &telegram.Document{FileID: "d", FileName: "big.iso", FileSize: 20<<20 + 1}
	ev := baseEvent(t, msg)
	bot := &fakeBot{files: map[string][]byte{"d": {1}}}
	up := &fakeUploader{}
	if err := AttachMedia(context.Background(), &ev, msg, bot, up); err != nil {
		t.Fatal(err)
	}
	if len(bot.downloaded) != 0 || len(up.uploads) != 0 {
		t.Fatalf("downloaded=%v uploads=%d, want neither", bot.downloaded, len(up.uploads))
	}
	assertUnsupported(t, ev, "document")
}

// The v1 DownloadFile truncates at the cap without saying so, so a body of exactly the cap is
// indistinguishable from a larger file that was cut: it must be refused, never uploaded.
func TestMediaInBodyAtTheDownloadCapIsRefused(t *testing.T) {
	msg := mediaMsg()
	msg.Voice = &telegram.Voice{FileID: "v"} // FileSize unknown (0)
	ev := baseEvent(t, msg)
	bot := &fakeBot{files: map[string][]byte{"v": bytes.Repeat([]byte{1}, maxDownloadBytes)}}
	up := &fakeUploader{}
	if err := AttachMedia(context.Background(), &ev, msg, bot, up); err != nil {
		t.Fatal(err)
	}
	if len(up.uploads) != 0 {
		t.Fatal("a possibly truncated body was uploaded")
	}
	assertUnsupported(t, ev, "voice")

	// One byte under the cap is a whole file.
	msg2 := mediaMsg()
	msg2.Voice = &telegram.Voice{FileID: "v"}
	ev2 := baseEvent(t, msg2)
	bot2 := &fakeBot{files: map[string][]byte{"v": bytes.Repeat([]byte{1}, maxDownloadBytes-1)}}
	up2 := &fakeUploader{}
	if err := AttachMedia(context.Background(), &ev2, msg2, bot2, up2); err != nil {
		t.Fatal(err)
	}
	if len(up2.uploads) != 1 {
		t.Fatalf("uploads = %d, want 1", len(up2.uploads))
	}
}

func TestMediaInFullMessageMatchesFixture(t *testing.T) {
	u := telegram.Update{UpdateID: 88123, Message: &telegram.Message{
		MessageID:       991,
		Chat:            telegram.Chat{ID: -100123, Type: "supergroup"},
		MessageThreadID: 5,
		IsTopicMessage:  true,
		From:            &telegram.User{ID: 4242, FirstName: "Duarte"},
		Date:            fixtureAt(t, eventFixture(t, "message-full")),
		Text:            "Vê isto e diz-me o que achas",
		Caption:         "legenda",
		ReplyToMessage:  &telegram.Message{MessageID: 987},
		Sticker:         &telegram.Sticker{FileID: "s"},
		Voice:           &telegram.Voice{FileID: "v", Duration: 4},
		Photo:           []telegram.PhotoSize{{FileID: "p", Width: 10, Height: 10}},
		Document:        &telegram.Document{FileID: "d", FileName: "x.pdf"},
	}}
	ev, needsMedia, ok := Inbound(u)
	if !ok || !needsMedia {
		t.Fatalf("ok=%v needsMedia=%v", ok, needsMedia)
	}
	bot := &fakeBot{files: map[string][]byte{"v": {1}, "p": {2}, "d": {3}}}
	up := &fakeUploader{next: 16} // the fixture's ids are m_17, m_18, m_19
	if err := AttachMedia(context.Background(), &ev, u.Message, bot, up); err != nil {
		t.Fatal(err)
	}

	want := eventFixture(t, "message-full")
	// A Telegram message has a single caption, shared by its image and file; the fixture gives each part
	// its own, and the 4100 ms voice is 4 whole seconds on the wire. The audio part has no caption field
	// in the contract, so it carries none.
	parts := append([]contract.InPart(nil), want.Message.Parts...)
	for i := range parts {
		switch *parts[i].Type {
		case contract.PartAudio:
			ms := uint64(4000)
			parts[i].DurationMS = &ms
		case contract.PartImage, contract.PartFile:
			parts[i].Caption = str("legenda")
		}
	}
	m := *want.Message
	m.Parts = parts
	want.Message = &m
	if !reflect.DeepEqual(ev, want) {
		gj, _ := json.MarshalIndent(ev, "", " ")
		wj, _ := json.MarshalIndent(want, "", " ")
		t.Fatalf("event differs:\n got %s\nwant %s", gj, wj)
	}
	assertDecodes(t, ev)
}

func assertUnsupported(t *testing.T, ev contract.Event, label string) {
	t.Helper()
	var found bool
	for _, p := range ev.Message.Parts {
		switch *p.Type {
		case contract.PartAudio, contract.PartImage, contract.PartFile:
			t.Fatalf("a failed media part leaked into the event: %+v", p)
		case contract.PartUnsupported:
			if *p.Label == label {
				found = true
			}
		}
	}
	if !found {
		t.Fatalf("no unsupported part labelled %q in %+v", label, ev.Message.Parts)
	}
	assertDecodes(t, ev)
}

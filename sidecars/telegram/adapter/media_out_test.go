package adapter

import (
	"context"
	"errors"
	"fmt"
	"io"
	"strings"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/kit"

	"nucleostelegram/telegram"
)

// SendMediaEx on fakeSendBot is a no-op so it satisfies SendBot; mediaBot overrides it.
func (f *fakeSendBot) SendMediaEx(context.Context, telegram.MediaKind, telegram.SendMediaParams, io.Reader) (telegram.MediaSent, error) {
	return telegram.MediaSent{}, nil
}

// mediaCall is one recorded media upload.
type mediaCall struct {
	kind   telegram.MediaKind
	params telegram.SendMediaParams
	body   string
}

// mediaBot records text and media sends in one ordered event list.
type mediaBot struct {
	fakeSendBot
	events   []string
	media    []mediaCall
	texts    []telegram.SendParams
	mediaErr error
	nextID   int64
}

func (b *mediaBot) id() int64 { b.nextID++; return 2000 + b.nextID }

func (b *mediaBot) SendHTMLEx(_ context.Context, p telegram.SendParams) (int64, error) {
	b.events = append(b.events, "text")
	b.texts = append(b.texts, p)
	return b.id(), nil
}

func (b *mediaBot) SendMediaEx(_ context.Context, kind telegram.MediaKind, p telegram.SendMediaParams, r io.Reader) (telegram.MediaSent, error) {
	data, _ := io.ReadAll(r)
	b.events = append(b.events, string(kind))
	b.media = append(b.media, mediaCall{kind, p, string(data)})
	if b.mediaErr != nil {
		return telegram.MediaSent{}, b.mediaErr
	}
	_, overflow := telegram.SplitCaption(p.Caption)
	return telegram.MediaSent{MessageID: b.id(), CaptionOverflow: overflow}, nil
}

// fakeDownloader serves bytes by media id and tracks that every reader was closed.
type fakeDownloader struct {
	data   map[string]string
	errs   map[string]error
	opened int
	closed int
}

type trackedReader struct {
	io.Reader
	d *fakeDownloader
}

func (t trackedReader) Close() error { t.d.closed++; return nil }

func (d *fakeDownloader) DownloadMedia(_ context.Context, id string, _ int64) (io.ReadCloser, error) {
	if err := d.errs[id]; err != nil {
		return nil, err
	}
	d.opened++
	return trackedReader{strings.NewReader(d.data[id]), d}, nil
}

func newDownloader() *fakeDownloader {
	return &fakeDownloader{
		data: map[string]string{"m_31": "IMG", "m_32": "PDF", "m_33": "OGG"},
		errs: map[string]error{},
	}
}

func reasonOf(t *testing.T, err error) (string, bool) {
	t.Helper()
	var ee *kit.ExecError
	if !errors.As(err, &ee) {
		t.Fatalf("err = %v, want *kit.ExecError", err)
	}
	return ee.Reason, ee.Retryable
}

func TestExecuteMediaSendFullInOrder(t *testing.T) {
	bot, dl := &mediaBot{}, newDownloader()
	refs, err := New(bot, dl).Execute(context.Background(), loadSend(t, "send-full"))
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.Join(bot.events, ","); got != "text,image,file,audio" {
		t.Fatalf("events = %s", got)
	}
	if len(refs) != 4 || refs[0] != "2001" || refs[3] != "2004" {
		t.Fatalf("refs = %v", refs)
	}
	img, file, aud := bot.media[0], bot.media[1], bot.media[2]
	if img.body != "IMG" || img.params.Caption != "diff" {
		t.Fatalf("image = %+v", img)
	}
	if file.kind != telegram.MediaFile || file.params.Filename != "relatorio.pdf" || file.params.Mime != "application/pdf" || file.body != "PDF" {
		t.Fatalf("file = %+v", file)
	}
	if aud.kind != telegram.MediaAudio || aud.params.Mime != "audio/ogg" || aud.body != "OGG" {
		t.Fatalf("audio = %+v", aud)
	}
	// reply_to only on the first message, silent and destination on all.
	if bot.texts[0].ReplyToMessageID != 980 || img.params.ReplyTo != 0 || file.params.ReplyTo != 0 {
		t.Fatalf("reply_to text=%d image=%d", bot.texts[0].ReplyToMessageID, img.params.ReplyTo)
	}
	for _, m := range bot.media {
		if !m.params.Silent || m.params.To != (telegram.Destination{ChatID: -100123, ThreadID: 5}) {
			t.Fatalf("media params = %+v", m.params)
		}
	}
	// Buttons ride on the text piece, not on a separate message.
	if len(bot.texts) != 1 || len(bot.texts[0].Buttons) != 1 {
		t.Fatalf("texts = %+v", bot.texts)
	}
	if dl.opened != 3 || dl.closed != 3 {
		t.Fatalf("opened=%d closed=%d", dl.opened, dl.closed)
	}
}

func TestExecuteMediaUnavailable(t *testing.T) {
	for name, cause := range map[string]error{
		"not found": kit.ErrMediaNotFound,
		"forbidden": fmt.Errorf("%w: download media: status 403", kit.ErrFatal),
	} {
		t.Run(name, func(t *testing.T) {
			bot, dl := &mediaBot{}, newDownloader()
			dl.errs["m_31"] = cause
			_, err := New(bot, dl).Execute(context.Background(), loadSend(t, "send-full"))
			if reason, retryable := reasonOf(t, err); reason != "media_unavailable" || retryable {
				t.Fatalf("reason=%q retryable=%v", reason, retryable)
			}
		})
	}
}

func TestExecuteMediaDownloadTransientIsRetryable(t *testing.T) {
	bot, dl := &mediaBot{}, newDownloader()
	dl.errs["m_31"] = fmt.Errorf("%w: download media: boom", kit.ErrTransient)
	_, err := New(bot, dl).Execute(context.Background(), loadSend(t, "send-full"))
	if reason, retryable := reasonOf(t, err); reason != "media_download" || !retryable {
		t.Fatalf("reason=%q retryable=%v", reason, retryable)
	}
}

func TestExecuteMediaTooLarge(t *testing.T) {
	t.Run("core says too large", func(t *testing.T) {
		bot, dl := &mediaBot{}, newDownloader()
		dl.errs["m_32"] = kit.ErrMediaTooLarge
		_, err := New(bot, dl).Execute(context.Background(), loadSend(t, "send-full"))
		if reason, retryable := reasonOf(t, err); reason != "media_too_large" || retryable {
			t.Fatalf("reason=%q retryable=%v", reason, retryable)
		}
	})
	t.Run("telegram refuses the size", func(t *testing.T) {
		bot, dl := &mediaBot{mediaErr: &telegram.APIError{Method: "sendDocument", StatusCode: 413, Description: "Request Entity Too Large"}}, newDownloader()
		_, err := New(bot, dl).Execute(context.Background(), loadSend(t, "send-full"))
		if reason, retryable := reasonOf(t, err); reason != "media_too_large" || retryable {
			t.Fatalf("reason=%q retryable=%v", reason, retryable)
		}
		if dl.closed != dl.opened {
			t.Fatalf("opened=%d closed=%d", dl.opened, dl.closed)
		}
	})
}

func TestExecuteMediaTelegramRateLimitIsRetryable(t *testing.T) {
	bot, dl := &mediaBot{mediaErr: &telegram.APIError{Method: "sendPhoto", StatusCode: 429}}, newDownloader()
	_, err := New(bot, dl).Execute(context.Background(), loadSend(t, "send-full"))
	if reason, retryable := reasonOf(t, err); reason != "rate_limited" || !retryable {
		t.Fatalf("reason=%q retryable=%v", reason, retryable)
	}
}

func TestExecuteMediaCaptionOverflowFollowUp(t *testing.T) {
	long := strings.Repeat("a", 1024) + "TAIL"
	s := contract.Send{ID: 1, Conversation: "42", Parts: []contract.OutPart{
		{Type: contract.OutImage, Media: "m_31", Caption: long},
		{Type: contract.OutFile, Media: "m_32", Name: "x.pdf", Mime: "application/pdf"},
	}}
	bot, dl := &mediaBot{}, newDownloader()
	refs, err := New(bot, dl).Execute(context.Background(), s)
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.Join(bot.events, ","); got != "image,text,file" {
		t.Fatalf("events = %s", got)
	}
	if len(refs) != 3 || len(bot.texts) != 1 || bot.texts[0].Text != "TAIL" || bot.texts[0].ParseMode != "" {
		t.Fatalf("refs=%v texts=%+v", refs, bot.texts)
	}
}

func TestExecuteMediaButtonsOwnMessageWhenNoText(t *testing.T) {
	s := contract.Send{ID: 1, Conversation: "42",
		Parts:   []contract.OutPart{{Type: contract.OutImage, Media: "m_31"}},
		Buttons: [][]contract.Button{{{Label: "Ok", Token: "tk_1"}}},
	}
	bot, dl := &mediaBot{}, newDownloader()
	refs, err := New(bot, dl).Execute(context.Background(), s)
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.Join(bot.events, ","); got != "image,text" || len(refs) != 2 {
		t.Fatalf("events=%s refs=%v", got, refs)
	}
	if len(bot.texts[0].Buttons) != 1 || bot.texts[0].Text == "" || len(bot.media[0].params.Caption) != 0 {
		t.Fatalf("texts=%+v", bot.texts)
	}
}

func TestExecuteMediaNoDownloaderIsPermanent(t *testing.T) {
	_, err := New(&mediaBot{}, nil).Execute(context.Background(), loadSend(t, "send-full"))
	if reason, _ := reasonOf(t, err); reason != "media_unavailable" {
		t.Fatalf("reason = %q", reason)
	}
}

package adapter

import (
	"context"
	"encoding/json"
	"fmt"
	"reflect"
	"strings"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
	"nucleoschannelkit/kit/adaptertest"

	"nucleostelegram/telegram"
)

// conformBot records every Bot API call the adapter makes: text and media sends (mediaBot) and
// edits, chat actions and callback answers (fakeMiscBot), each in its own ordered list.
type conformBot struct {
	mediaBot
	misc fakeMiscBot
}

func (b *conformBot) EditMessageText(ctx context.Context, chatID, messageID int64, text, parseMode string, buttons [][]telegram.Button) error {
	return b.misc.EditMessageText(ctx, chatID, messageID, text, parseMode, buttons)
}

func (b *conformBot) SendChatAction(ctx context.Context, to telegram.Destination, action string) error {
	return b.misc.SendChatAction(ctx, to, action)
}

func (b *conformBot) AnswerCallbackQueryCtx(ctx context.Context, id, text string) error {
	return b.misc.AnswerCallbackQueryCtx(ctx, id, text)
}

// recordTB captures what a helper reports through Error/Log instead of failing the real test, so a
// known conflict between the adapter and the shared fixtures can be asserted exactly.
type recordTB struct {
	testing.TB
	errs []string
}

func (r *recordTB) Error(args ...any)            { r.errs = append(r.errs, fmt.Sprint(args...)) }
func (r *recordTB) Errorf(f string, args ...any) { r.errs = append(r.errs, fmt.Sprintf(f, args...)) }

func TestConformanceCapabilities(t *testing.T) {
	adaptertest.CheckCapabilities(t, Capabilities())
}

// The shared outbox/edit.json holds one text piece over two target refs, which the adapter refuses
// with edit_unsupported_shape so the core falls back to a fresh send; adaptertest.ExecuteValid
// accepts that refusal on a shape mismatch, so every valid action reports clean.
func TestConformanceExecuteValid(t *testing.T) {
	bot, dl := &conformBot{}, newDownloader()
	rec := &recordTB{TB: t}
	adaptertest.ExecuteValid(rec, New(bot, dl))

	if len(rec.errs) != 0 {
		t.Fatalf("ExecuteValid reports = %q, want none", rec.errs)
	}

	// send-full: text, image, file, audio in order; then send-text and send-unknown-field (valid for
	// the Go decoder, canonical send-text) one text each.
	if got := strings.Join(bot.events, ","); got != "text,image,file,audio,text,text" {
		t.Fatalf("send events = %s", got)
	}
	if len(bot.texts) != 3 || bot.texts[0].ReplyToMessageID != 980 || len(bot.texts[0].Buttons) != 1 ||
		bot.texts[1].Text != "Done." || bot.texts[1].To != (telegram.Destination{ChatID: 4242}) || bot.texts[2].Text != "Done." {
		t.Fatalf("texts = %+v", bot.texts)
	}
	if len(bot.media) != 3 || bot.media[0].body != "IMG" || bot.media[1].body != "PDF" || bot.media[2].body != "OGG" {
		t.Fatalf("media = %+v", bot.media)
	}
	if dl.opened != 3 || dl.closed != 3 {
		t.Fatalf("opened=%d closed=%d", dl.opened, dl.closed)
	}

	// answer-action: the callback id is the action_ref.
	if len(bot.misc.answers) != 1 || bot.misc.answers[0] != (answerCall{"551", "Already handled."}) {
		t.Fatalf("answers = %+v", bot.misc.answers)
	}
	// The conflicting edit must not have reached the Bot API.
	if len(bot.misc.edits) != 0 {
		t.Fatalf("edits = %+v, want none", bot.misc.edits)
	}
}

// The edit action the fixture describes, with the refs the adapter's rule requires (one per piece),
// executes against the Bot API with the expected call.
func TestConformanceExecuteEditWithMatchingRefs(t *testing.T) {
	e, ok := loadAction(t, "edit").(contract.Edit)
	if !ok {
		t.Fatal("outbox/edit.json is not an edit")
	}
	e.TargetPlatformRefs = e.TargetPlatformRefs[:1]
	bot := &conformBot{}
	refs, err := New(bot, newDownloader()).Execute(context.Background(), e)
	if err != nil {
		t.Fatal(err)
	}
	if len(refs) != 1 || refs[0] != "993" {
		t.Fatalf("refs = %v", refs)
	}
	if len(bot.misc.edits) != 1 {
		t.Fatalf("edits = %+v", bot.misc.edits)
	}
	if c := bot.misc.edits[0]; c.chatID != -100123 || c.messageID != 993 || c.parseMode != "HTML" ||
		!strings.Contains(c.text, "Proposal 12") {
		t.Fatalf("edit call = %+v", c)
	}
}

// Typing is the one capability with no outbox action fixture: it travels as an ephemeral. The
// declared capability must be executable.
func TestConformanceTyping(t *testing.T) {
	bot := &conformBot{}
	if err := New(bot, nil).Typing(context.Background(), "-100123:5"); err != nil {
		t.Fatal(err)
	}
	if len(bot.misc.actions) != 1 || bot.misc.actions[0] != (actionCall{telegram.Destination{ChatID: -100123, ThreadID: 5}, "typing"}) {
		t.Fatalf("actions = %+v", bot.misc.actions)
	}
}

// conformEvents builds, per valid events/ fixture, the event the adapter produces from the
// equivalent Telegram update. A valid fixture missing here fails the test below, so a new fixture
// cannot go unexercised.
var conformEvents = map[string]func(t *testing.T) contract.Event{
	"events/message-private-text.json": func(t *testing.T) contract.Event {
		ev, _, ok := Inbound(privateText(88124, 992, "/kill on"))
		if !ok {
			t.Fatal("dropped")
		}
		return ev
	},
	"events/message-pair.json": func(t *testing.T) contract.Event {
		ev, _, ok := Inbound(privateText(88126, 993, "/pair 483920"))
		if !ok {
			t.Fatal("dropped")
		}
		return ev
	},
	"events/message-group-image.json": func(t *testing.T) contract.Event {
		msg := &telegram.Message{
			MessageID: 12,
			Chat:      telegram.Chat{ID: -100777, Type: "group"},
			From:      &telegram.User{ID: 4242, FirstName: "Duarte"},
			Date:      time.Date(2026, 10, 19, 9, 31, 0, 0, time.UTC).Unix(),
			Photo:     []telegram.PhotoSize{{FileID: "p", Width: 10, Height: 10}},
		}
		ev, needsMedia, ok := Inbound(telegram.Update{UpdateID: 88125, Message: msg})
		if !ok || !needsMedia {
			t.Fatalf("ok=%v needsMedia=%v", ok, needsMedia)
		}
		bot := &fakeBot{files: map[string][]byte{"p": {2}}}
		if err := AttachMedia(context.Background(), &ev, msg, bot, &fakeUploader{next: 19}); err != nil {
			t.Fatal(err)
		}
		return ev
	},
	"events/message-full.json": func(t *testing.T) contract.Event {
		msg := &telegram.Message{
			MessageID:       991,
			Chat:            telegram.Chat{ID: -100123, Type: "supergroup"},
			MessageThreadID: 5,
			IsTopicMessage:  true,
			From:            &telegram.User{ID: 4242},
			Text:            "Vê isto e diz-me o que achas",
			Caption:         "legenda",
			ReplyToMessage:  &telegram.Message{MessageID: 987},
			Sticker:         &telegram.Sticker{FileID: "s"},
			Voice:           &telegram.Voice{FileID: "v", Duration: 4},
			Photo:           []telegram.PhotoSize{{FileID: "p", Width: 10, Height: 10}},
			Document:        &telegram.Document{FileID: "d", FileName: "x.pdf"},
		}
		ev, needsMedia, ok := Inbound(telegram.Update{UpdateID: 88123, Message: msg})
		if !ok || !needsMedia {
			t.Fatalf("ok=%v needsMedia=%v", ok, needsMedia)
		}
		bot := &fakeBot{files: map[string][]byte{"v": {1}, "p": {2}, "d": {3}}}
		if err := AttachMedia(context.Background(), &ev, msg, bot, &fakeUploader{next: 16}); err != nil {
			t.Fatal(err)
		}
		return ev
	},
	"events/action.json": func(t *testing.T) contract.Event {
		ev, ok, reason := InboundCallback(callbackQuery("tk_9f2c"))
		if !ok {
			t.Fatalf("dropped: %s", reason)
		}
		return ev
	},
}

// The group-image fixture spells its "at" with a +01:00 offset; the adapter emits the same instant in UTC,
// and carries the sender's name.
func TestConformanceGroupImageMatchesFixture(t *testing.T) {
	got := conformEvents["events/message-group-image.json"](t)
	want := utcAt(t, eventFixture(t, "message-group-image"))
	// The fixture gives the image a mime that message-full's image lacks; the adapter sends none.
	parts := append([]contract.InPart(nil), want.Message.Parts...)
	parts[0].Mime = nil
	m := *want.Message
	m.Parts = parts
	want.Message = &m
	if !reflect.DeepEqual(got, want) {
		gj, _ := json.MarshalIndent(got, "", " ")
		wj, _ := json.MarshalIndent(want, "", " ")
		t.Fatalf("event differs:\n got %s\nwant %s", gj, wj)
	}
}

func TestConformanceInboundEventsDecodeStrict(t *testing.T) {
	idx, err := fixtures.Index()
	if err != nil {
		t.Fatal(err)
	}
	seen := 0
	for _, e := range idx.Fixtures {
		// event_response fixtures are core replies, not events the adapter produces.
		if e.Shape != "event" || !strings.HasPrefix(e.File, "events/") || !e.Valid {
			continue
		}
		seen++
		build, ok := conformEvents[e.File]
		if !ok {
			t.Errorf("%s: valid event fixture with no equivalent Telegram update in conformEvents", e.File)
			continue
		}
		t.Run(strings.TrimSuffix(strings.TrimPrefix(e.File, "events/"), ".json"), func(t *testing.T) {
			ev := build(t)
			assertDecodes(t, ev)
			if verr := ev.Validate(); verr != nil {
				t.Fatalf("Validate: %v", verr)
			}
		})
	}
	if seen != len(conformEvents) {
		t.Errorf("%d valid event fixtures, %d builders: a builder has no fixture", seen, len(conformEvents))
	}
}

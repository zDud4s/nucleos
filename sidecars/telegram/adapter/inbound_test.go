package adapter

import (
	"io/fs"
	"reflect"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"

	"nucleostelegram/telegram"
)

func eventFixture(t *testing.T, name string) contract.Event {
	t.Helper()
	b, err := fs.ReadFile(fixtures.FS(), "events/"+name+".json")
	if err != nil {
		t.Fatal(err)
	}
	ev, err := contract.DecodeStrict[contract.Event](b)
	if err != nil {
		t.Fatal(err)
	}
	return ev
}

// fixtureAt is the instant of the fixture's "at" as Telegram would send it: whole unix seconds.
func fixtureAt(t *testing.T, ev contract.Event) int64 {
	t.Helper()
	at, err := time.Parse(time.RFC3339, *ev.At)
	if err != nil {
		t.Fatal(err)
	}
	return at.Unix()
}

// utcAt rewrites the fixture's "at" to RFC3339 UTC, the one form the adapter emits, whatever offset the
// fixture spells it with.
func utcAt(t *testing.T, ev contract.Event) contract.Event {
	t.Helper()
	at := time.Unix(fixtureAt(t, ev), 0).UTC().Format(time.RFC3339)
	ev.At = &at
	return ev
}

func privateText(updateID, messageID int64, text string) telegram.Update {
	return telegram.Update{UpdateID: updateID, Message: &telegram.Message{
		MessageID: messageID,
		Chat:      telegram.Chat{ID: 4242, Type: "private"},
		From:      &telegram.User{ID: 4242},
		Text:      text,
	}}
}

func withFirstName(u telegram.Update, name string) telegram.Update {
	from := *u.Message.From
	from.FirstName = name
	u.Message.From = &from
	return u
}

func TestInboundSenderNameComposition(t *testing.T) {
	cases := []struct {
		first, last, user string
		want              *string
	}{
		{"Duarte", "", "", str("Duarte")},
		{"Duarte", "Ferreira", "dudas", str("Duarte Ferreira")},
		{"", "Ferreira", "", str("Ferreira")},
		{"", "", "dudas", str("dudas")},
		{"  ", "", "", nil},
		{"", "", "", nil},
	}
	for _, c := range cases {
		u := privateText(1, 2, "hi")
		u.Message.From.FirstName, u.Message.From.LastName, u.Message.From.Username = c.first, c.last, c.user
		got, _, ok := Inbound(u)
		if !ok {
			t.Fatal("dropped")
		}
		switch {
		case c.want == nil && got.Sender.Name != nil:
			t.Errorf("%+v: name = %q, want none", c, *got.Sender.Name)
		case c.want != nil && (got.Sender.Name == nil || *got.Sender.Name != *c.want):
			t.Errorf("%+v: name = %v, want %q", c, got.Sender.Name, *c.want)
		}
		assertDecodes(t, got)
	}
}

func TestInboundAtIsRFC3339UTCFromUnixDate(t *testing.T) {
	u := privateText(1, 2, "hi")
	u.Message.Date = time.Date(2026, 10, 19, 10, 31, 0, 0, time.FixedZone("x", 3600)).Unix()
	got, _, _ := Inbound(u)
	if got.At == nil || *got.At != "2026-10-19T09:31:00Z" {
		t.Fatalf("at = %v", got.At)
	}
	assertDecodes(t, got)
	if none, _, _ := Inbound(privateText(1, 2, "hi")); none.At != nil {
		t.Fatalf("a message without a date must carry no at, got %q", *none.At)
	}
}

func TestInboundTextPrivate(t *testing.T) {
	got, needsMedia, ok := Inbound(privateText(88124, 992, "/kill on"))
	if !ok || needsMedia {
		t.Fatalf("ok=%v needsMedia=%v", ok, needsMedia)
	}
	if want := eventFixture(t, "message-private-text"); !reflect.DeepEqual(got, want) {
		t.Fatalf("event differs:\n got %+v\nwant %+v", got, want)
	}
}

func TestInboundTextPairPassesRaw(t *testing.T) {
	got, _, ok := Inbound(withFirstName(privateText(88126, 993, "/pair 483920"), "Duarte"))
	if !ok {
		t.Fatal("dropped")
	}
	want := eventFixture(t, "message-pair")
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("event differs:\n got %+v\nwant %+v", got, want)
	}
}

func TestInboundTextNoFromDropped(t *testing.T) {
	u := privateText(1, 2, "hi")
	u.Message.From = nil
	if _, _, ok := Inbound(u); ok {
		t.Fatal("a message without from must be dropped")
	}
	if _, _, ok := Inbound(telegram.Update{UpdateID: 3}); ok {
		t.Fatal("an update without message must be dropped")
	}
}

func TestInboundThreadWithReply(t *testing.T) {
	u := telegram.Update{UpdateID: 88123, Message: &telegram.Message{
		MessageID:       991,
		Chat:            telegram.Chat{ID: -100123, Type: "supergroup"},
		MessageThreadID: 5,
		IsTopicMessage:  true,
		From:            &telegram.User{ID: 4242, FirstName: "Duarte"},
		Date:            fixtureAt(t, eventFixture(t, "message-full")),
		Text:            "Vê isto e diz-me o que achas",
		ReplyToMessage:  &telegram.Message{MessageID: 987},
		Sticker:         &telegram.Sticker{FileID: "s"},
	}}
	got, _, ok := Inbound(u)
	if !ok {
		t.Fatal("dropped")
	}
	want := eventFixture(t, "message-full")
	// Media parts are attached by AttachMedia, not by Inbound.
	var parts []contract.InPart
	for _, p := range want.Message.Parts {
		switch *p.Type {
		case contract.PartAudio, contract.PartImage, contract.PartFile:
		default:
			parts = append(parts, p)
		}
	}
	m := *want.Message
	m.Parts = parts
	want.Message = &m
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("event differs:\n got %+v\nwant %+v", got, want)
	}
}

func TestInboundThreadRequiresTopicFlag(t *testing.T) {
	u := telegram.Update{UpdateID: 1, Message: &telegram.Message{
		MessageID:       10,
		Chat:            telegram.Chat{ID: -100123, Type: "supergroup"},
		MessageThreadID: 77, // a plain reply chain, not a forum topic
		From:            &telegram.User{ID: 4242},
		Text:            "reply",
		ReplyToMessage:  &telegram.Message{MessageID: 9},
	}}
	got, _, ok := Inbound(u)
	if !ok {
		t.Fatal("dropped")
	}
	c := got.Conversation
	if *c.Kind != contract.ConvGroup || *c.Ref != "-100123" || c.Parent != nil {
		t.Fatalf("conversation = %+v, want bare group", c)
	}
}

func TestInboundGroupNoTopic(t *testing.T) {
	u := telegram.Update{UpdateID: 1, Message: &telegram.Message{
		MessageID: 12,
		Chat:      telegram.Chat{ID: -100777, Type: "group"},
		From:      &telegram.User{ID: 4242},
		Text:      "hello",
	}}
	got, _, ok := Inbound(u)
	if !ok {
		t.Fatal("dropped")
	}
	if *got.Conversation.Kind != contract.ConvGroup || *got.Conversation.Ref != "-100777" {
		t.Fatalf("conversation = %+v", got.Conversation)
	}
}

func TestInboundUnsupportedLabels(t *testing.T) {
	cases := map[string]func(m *telegram.Message){
		"sticker":    func(m *telegram.Message) { m.Sticker = &telegram.Sticker{} },
		"video":      func(m *telegram.Message) { m.Video = &telegram.Video{} },
		"video_note": func(m *telegram.Message) { m.VideoNote = &telegram.VideoNote{} },
		"animation":  func(m *telegram.Message) { m.Animation = &telegram.Animation{} },
		"contact":    func(m *telegram.Message) { m.Contact = &telegram.Contact{} },
		"location":   func(m *telegram.Message) { m.Location = &telegram.Location{} },
		"poll":       func(m *telegram.Message) { m.Poll = &telegram.Poll{} },
	}
	for label, set := range cases {
		u := privateText(1, 2, "")
		set(u.Message)
		got, needsMedia, ok := Inbound(u)
		if !ok || needsMedia {
			t.Fatalf("%s: ok=%v needsMedia=%v", label, ok, needsMedia)
		}
		parts := got.Message.Parts
		if len(parts) != 1 || *parts[0].Type != contract.PartUnsupported || *parts[0].Label != label {
			t.Fatalf("%s: parts = %+v", label, parts)
		}
		if err := got.Validate(); err != nil {
			t.Fatalf("%s: Validate: %v", label, err)
		}
	}
}

func TestInboundMediaOnlyPhotoNoCaption(t *testing.T) {
	u := privateText(1, 2, "")
	u.Message.Photo = []telegram.PhotoSize{{FileID: "p"}}
	got, needsMedia, ok := Inbound(u)
	if !ok || !needsMedia {
		t.Fatalf("ok=%v needsMedia=%v", ok, needsMedia)
	}
	if len(got.Message.Parts) != 0 {
		t.Fatalf("parts = %+v, want none", got.Message.Parts)
	}
}

func TestInboundMediaOnlyVoiceWithCaption(t *testing.T) {
	u := privateText(1, 2, "")
	u.Message.Voice = &telegram.Voice{FileID: "v"}
	u.Message.Caption = "listen"
	got, needsMedia, ok := Inbound(u)
	if !ok || !needsMedia {
		t.Fatalf("ok=%v needsMedia=%v", ok, needsMedia)
	}
	parts := got.Message.Parts
	if len(parts) != 1 || *parts[0].Type != contract.PartText || *parts[0].Text != "listen" {
		t.Fatalf("parts = %+v", parts)
	}
}

func TestInboundEmptyDropped(t *testing.T) {
	if _, _, ok := Inbound(privateText(1, 2, "")); ok {
		t.Fatal("a message with neither text nor media must be dropped")
	}
}

package pipe

import (
	"bytes"
	"errors"
	"log"
	"os"
	"strings"
	"testing"

	"nucleostelegram/config"
	"nucleostelegram/telegram"
)

func botRequest(text string) *telegram.Message {
	return &telegram.Message{Chat: telegram.Chat{ID: 42}, From: &telegram.User{ID: 999, IsBot: true}, Text: text}
}

func replyUpdate(reply *telegram.Message, to *telegram.Message) telegram.Update {
	reply.Chat = telegram.Chat{ID: 42}
	reply.From = &telegram.User{ID: 42}
	reply.ReplyToMessage = to
	return telegram.Update{Message: reply}
}

func TestAReplyToACaptureRequestAnswersItWithoutATurn(t *testing.T) {
	bot, dc := &recordingBot{}, &recordingDaemon{answerReleased: true}
	HandleUpdate(bot, dc, fakeDownloader{}, config.Config{AllowedChatID: 42}, NewTracker(),
		replyUpdate(&telegram.Message{Text: "it was the VPN"}, botRequest("📣 🧠 web · job #7 · failed\n… #cap7")))

	if len(dc.answerCalls) != 1 || dc.answerCalls[0] != (answerCall{7, "it was the VPN"}) {
		t.Fatalf("answerCalls = %v, want one for job 7", dc.answerCalls)
	}
	if dc.sendAssistantCalls != 0 {
		t.Errorf("SendAssistantMessage calls = %d, want 0", dc.sendAssistantCalls)
	}
	if len(bot.messages) != 1 || bot.messages[0].text != "noted (#31), linked to job #7" {
		t.Errorf("messages = %v", bot.messages)
	}
}

func TestALateAnswerSaysTheDistillerMovedOn(t *testing.T) {
	bot, dc := &recordingBot{}, &recordingDaemon{answerReleased: false}
	HandleUpdate(bot, dc, fakeDownloader{}, config.Config{AllowedChatID: 42}, NewTracker(),
		replyUpdate(&telegram.Message{Text: "late"}, botRequest("… #cap7")))

	if len(bot.messages) != 1 || bot.messages[0].text != "noted (#31); the distiller already moved on for this job" {
		t.Errorf("messages = %v", bot.messages)
	}
}

func TestAReplyWithoutTheMarkOrNotToABotIsAnOrdinaryTurn(t *testing.T) {
	for name, to := range map[string]*telegram.Message{
		"bot message without mark": botRequest("📣 job_failed: something"),
		"human message with mark":  {Chat: telegram.Chat{ID: 42}, From: &telegram.User{ID: 42}, Text: "#cap7"},
		"no author":                {Chat: telegram.Chat{ID: 42}, Text: "#cap7"},
	} {
		t.Run(name, func(t *testing.T) {
			dc := &recordingDaemon{}
			HandleUpdate(&recordingBot{}, dc, fakeDownloader{}, config.Config{AllowedChatID: 42}, NewTracker(),
				replyUpdate(&telegram.Message{Text: "hello"}, to))
			if len(dc.answerCalls) != 0 {
				t.Errorf("answerCalls = %v, want none", dc.answerCalls)
			}
			if dc.sendAssistantCalls != 1 {
				t.Errorf("SendAssistantMessage calls = %d, want 1", dc.sendAssistantCalls)
			}
		})
	}
}

func TestADictatedAttachedOrEmptyReplyToACaptureRequestIsRefused(t *testing.T) {
	for name, reply := range map[string]*telegram.Message{
		"voice":    {Voice: &telegram.Voice{FileID: "v"}},
		"photo":    {Photo: []telegram.PhotoSize{{FileID: "p"}}, Caption: "look"},
		"document": {Document: &telegram.Document{FileID: "d"}},
		"sticker":  {},
	} {
		t.Run(name, func(t *testing.T) {
			bot, dc := &recordingBot{}, &recordingDaemon{}
			HandleUpdate(bot, dc, fakeDownloader{}, config.Config{AllowedChatID: 42}, NewTracker(),
				replyUpdate(reply, botRequest("… #cap7")))
			if len(dc.answerCalls) != 0 || dc.sendAssistantCalls != 0 {
				t.Errorf("answer=%v turns=%d, want neither", dc.answerCalls, dc.sendAssistantCalls)
			}
			if len(bot.messages) != 1 || bot.messages[0].text != captureTypedOnly {
				t.Errorf("messages = %v", bot.messages)
			}
		})
	}
}

func TestAReplyFromAnUnauthorisedSenderNeverAnswers(t *testing.T) {
	dc := &recordingDaemon{}
	u := replyUpdate(&telegram.Message{Text: "x"}, botRequest("… #cap7"))
	u.Message.From = &telegram.User{ID: 5}
	HandleUpdate(&recordingBot{}, dc, fakeDownloader{}, config.Config{AllowedChatID: 42}, NewTracker(), u)
	if len(dc.answerCalls) != 0 {
		t.Errorf("answerCalls = %v, want none", dc.answerCalls)
	}
}

func TestACaptureAnswerNeverReachesTheLog(t *testing.T) {
	const secret = "the-secret-capture-answer"
	var buf bytes.Buffer
	log.SetOutput(&buf)
	defer log.SetOutput(os.Stderr)

	for _, dc := range []*recordingDaemon{{}, {answerErr: errors.New("daemon unreachable")}} {
		HandleUpdate(&recordingBot{}, dc, fakeDownloader{}, config.Config{AllowedChatID: 42}, NewTracker(),
			replyUpdate(&telegram.Message{Text: secret}, botRequest("… #cap7")))
	}
	if strings.Contains(buf.String(), secret) {
		t.Errorf("log carries the answer text: %q", buf.String())
	}
}

func TestCaptureMarkParsing(t *testing.T) {
	for text, want := range map[string]int64{
		"… #cap7": 7, "#cap123 tail": 123, "no mark": 0, "#capx": 0, "#cap": 0, "#cap12abc": 0,
		"«#cap3 in a description» … #cap7": 7,
	} {
		if got := captureMark(text); got != want {
			t.Errorf("captureMark(%q) = %d, want %d", text, got, want)
		}
	}
}

// hints returns the capture hints among the messages the bot sent, in order.
func hints(bot *recordingBot) []string {
	var out []string
	for _, m := range bot.messages {
		if strings.Contains(m.text, "#cap") {
			out = append(out, m.text)
		}
	}
	return out
}

func looseUpdate(text string) telegram.Update {
	return telegram.Update{Message: &telegram.Message{Chat: telegram.Chat{ID: 42}, From: &telegram.User{ID: 42}, Text: text}}
}

func TestALooseMessageWithARequestOpenIsHintedOnceAndStillReachesTheAssistant(t *testing.T) {
	// The send is refused so no turn starts: a turn polls this fake from its own goroutine, and the
	// test only needs to know the message was handed to the assistant.
	bot, tr := &recordingBot{}, NewTracker()
	dc := &recordingDaemon{openCaptures: []int64{29, 31}, sendAssistantErr: errors.New("busy")}
	cfg := config.Config{AllowedChatID: 42}
	HandleUpdate(bot, dc, fakeDownloader{}, cfg, tr, looseUpdate("the VPN was down"))
	HandleUpdate(bot, dc, fakeDownloader{}, cfg, tr, looseUpdate("and another thing"))

	want := "This message went to the assistant, not to a capture request. To answer one, use Reply on its message (#cap29, #cap31)."
	if got := hints(bot); len(got) != 1 || got[0] != want {
		t.Errorf("hints = %v, want the hint once", got)
	}
	if len(dc.answerCalls) != 0 {
		t.Errorf("answerCalls = %v, want none: a loose message is never an answer", dc.answerCalls)
	}
	if dc.sendAssistantCalls != 2 {
		t.Errorf("SendAssistantMessage calls = %d, want 2", dc.sendAssistantCalls)
	}

	// A request opened later is hinted on its own.
	dc.openCaptures = []int64{29, 31, 40}
	HandleUpdate(bot, dc, fakeDownloader{}, cfg, tr, looseUpdate("hi"))
	if got := hints(bot); len(got) != 2 || !strings.HasSuffix(got[1], "(#cap40).") {
		t.Errorf("hints = %v, want a second hint naming only #cap40", got)
	}
}

func TestNoHintWithoutAnOpenRequestForACommandOrWhenTheListingFails(t *testing.T) {
	cfg := config.Config{AllowedChatID: 42}
	for name, c := range map[string]struct {
		dc   *recordingDaemon
		text string
	}{
		"nothing open":    {&recordingDaemon{sendAssistantErr: errors.New("busy")}, "hello"},
		"command":         {&recordingDaemon{openCaptures: []int64{29}}, "/help"},
		"listing failure": {&recordingDaemon{openCaptures: []int64{29}, openCapturesErr: errors.New("down"), sendAssistantErr: errors.New("busy")}, "hello"},
	} {
		t.Run(name, func(t *testing.T) {
			bot := &recordingBot{}
			HandleUpdate(bot, c.dc, fakeDownloader{}, cfg, NewTracker(), looseUpdate(c.text))
			if got := hints(bot); len(got) != 0 {
				t.Errorf("hints sent: %v", got)
			}
		})
	}
}

func TestAReplyToACaptureRequestIsNeverHinted(t *testing.T) {
	bot, dc := &recordingBot{}, &recordingDaemon{answerReleased: true, openCaptures: []int64{7}}
	HandleUpdate(bot, dc, fakeDownloader{}, config.Config{AllowedChatID: 42}, NewTracker(),
		replyUpdate(&telegram.Message{Text: "it was the VPN"}, botRequest("… #cap7")))
	if dc.openCaptureCalls != 0 || len(bot.messages) != 1 {
		t.Errorf("listing calls = %d, messages = %v, want the answer alone", dc.openCaptureCalls, bot.messages)
	}
}

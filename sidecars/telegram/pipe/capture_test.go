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
	} {
		if got := captureMark(text); got != want {
			t.Errorf("captureMark(%q) = %d, want %d", text, got, want)
		}
	}
}

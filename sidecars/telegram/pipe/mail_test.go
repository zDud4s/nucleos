package pipe

import (
	"nucleostelegram/telegram"
	"strings"
	"testing"
)

// `/mail` is the only shortcut that spends money, and that is the whole design: mail is collected
// in the background because collecting is free, and classifying waits to be asked for.
func TestMailShortcutTriagesOnceAndSaysWhatItStarted(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{}

	HandleMessage(bot, dc, NewTracker(), telegram.Destination{ChatID: 42}, "/mail")

	if dc.triageCalls != 1 {
		t.Fatalf("TriageEmail calls = %d, want exactly 1", dc.triageCalls)
	}
	if dc.sendAssistantCalls != 0 {
		t.Errorf("a shortcut must not go through the orchestrator: %d call(s)", dc.sendAssistantCalls)
	}
	if len(bot.messages) != 1 || !strings.Contains(bot.messages[0].text, "3 message") {
		t.Errorf("messages = %v, want the count it started with", bot.messages)
	}
}

// The counterpart, and the reason both exist: looking must never cost anything, or a person stops
// looking.
func TestInboxShortcutSpendsNothing(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{emailQueue: []map[string]any{
		{"from_name": "Rui", "subject": "produção em baixo", "triage_class": nil},
		{"from_name": "Ana", "subject": "notas", "triage_class": "info", "triage_summary": "sem ação"},
	}}

	HandleMessage(bot, dc, NewTracker(), telegram.Destination{ChatID: 42}, "/inbox")

	if dc.triageCalls != 0 {
		t.Fatalf("looking at the inbox must not trigger a run: %d call(s)", dc.triageCalls)
	}
	if len(bot.messages) != 1 {
		t.Fatalf("messages = %v, want one", bot.messages)
	}
	text := bot.messages[0].text
	if !strings.Contains(text, "waiting to be read (1)") {
		t.Errorf("reply = %q, want the waiting count", text)
	}
	if !strings.Contains(text, "produção em baixo") {
		t.Errorf("reply = %q, want the waiting subject", text)
	}
	if !strings.Contains(text, "[info]") || !strings.Contains(text, "sem ação") {
		t.Errorf("reply = %q, want the verdict already reached", text)
	}
}

// An empty mailbox is a normal state, not an error, and must not read like one.
func TestInboxWithNothingSaysSo(t *testing.T) {
	bot := &recordingBot{}
	HandleMessage(bot, &recordingDaemon{}, NewTracker(), telegram.Destination{ChatID: 42}, "/inbox")

	if len(bot.messages) != 1 || !strings.Contains(bot.messages[0].text, "nothing in the mailbox") {
		t.Errorf("messages = %v, want a plain empty-mailbox reply", bot.messages)
	}
}

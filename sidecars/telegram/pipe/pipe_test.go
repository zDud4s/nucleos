package pipe

import (
	"errors"
	"strings"
	"testing"
	"time"

	"nucleostelegram/telegram"
)

type sentMessage struct {
	chatID int64
	text   string
}

type callbackAnswer struct {
	id   string
	text string
}

type recordingBot struct {
	messages     []sentMessage
	htmlMessages []sentMessage
	htmlErr      error
	answers      []callbackAnswer
}

func (b *recordingBot) SendMessage(chatID int64, text string) error {
	b.messages = append(b.messages, sentMessage{chatID: chatID, text: text})
	return nil
}

func (b *recordingBot) SendHTML(chatID int64, html string) error {
	b.htmlMessages = append(b.htmlMessages, sentMessage{chatID: chatID, text: html})
	return b.htmlErr
}

func (b *recordingBot) SendMessageWithButtons(chatID int64, text string, _ [][]telegram.Button) error {
	b.messages = append(b.messages, sentMessage{chatID: chatID, text: text})
	return nil
}

func (b *recordingBot) AnswerCallbackQuery(callbackID, text string) error {
	b.answers = append(b.answers, callbackAnswer{id: callbackID, text: text})
	return nil
}

type recordingDaemon struct {
	sendAssistantErr   error
	sendAssistantCalls int
	run                map[string]any
	runErr             error
	projects           []map[string]any
	projectsErr        error
	getProjectsCalls   int
	setKillCalls       []bool
	cancelCalls        []int64
	approveCalls       []int64
	triageCalls        int
	emailQueue         []map[string]any
}

type fakeDownloader struct {
	remotePath string
	data       []byte
}

func (d fakeDownloader) GetFile(string) (string, error) {
	return d.remotePath, nil
}

func (d fakeDownloader) DownloadFile(string) ([]byte, error) {
	return d.data, nil
}

func (d *recordingDaemon) SendAssistantMessage(string, string) (int64, error) {
	d.sendAssistantCalls++
	return 0, d.sendAssistantErr
}

func (d *recordingDaemon) GetRun(int64) (map[string]any, error) {
	return d.run, d.runErr
}

func (d *recordingDaemon) GetProposals() ([]map[string]any, error) {
	return nil, nil
}

func (d *recordingDaemon) GetProjects() ([]map[string]any, error) {
	d.getProjectsCalls++
	return d.projects, d.projectsErr
}

func (d *recordingDaemon) ApproveProposal(id int64) (map[string]any, error) {
	d.approveCalls = append(d.approveCalls, id)
	return map[string]any{}, nil
}

func (d *recordingDaemon) RejectProposal(int64) error {
	return nil
}

func (d *recordingDaemon) GetFeed() ([]map[string]any, error) {
	return nil, nil
}

func (d *recordingDaemon) GetBudget() (map[string]any, error) {
	return map[string]any{}, nil
}

func (d *recordingDaemon) GetKill() (bool, error) {
	return false, nil
}

// Recorded rather than stubbed silently: triaging is the one shortcut that spends money, so a test
// that accidentally reaches it should be able to notice.
func (d *recordingDaemon) TriageEmail() (map[string]any, error) {
	d.triageCalls++
	return map[string]any{"queued": float64(3), "run_id": float64(77)}, nil
}

func (d *recordingDaemon) GetEmailQueue() ([]map[string]any, error) {
	return d.emailQueue, nil
}

func (d *recordingDaemon) SetKill(engaged bool) error {
	d.setKillCalls = append(d.setKillCalls, engaged)
	return nil
}

func (d *recordingDaemon) CancelRun(id int64) error {
	d.cancelCalls = append(d.cancelCalls, id)
	return nil
}

func TestParseCallback(t *testing.T) {
	tests := []struct {
		data       string
		wantAction string
		wantID     int64
		wantOK     bool
	}{
		{data: "approve:12", wantAction: "approve", wantID: 12, wantOK: true},
		{data: "reject:3", wantAction: "reject", wantID: 3, wantOK: true},
		{data: "garbage", wantOK: false},
		{data: "approve:x", wantOK: false},
	}

	for _, tt := range tests {
		t.Run(tt.data, func(t *testing.T) {
			action, id, ok := parseCallback(tt.data)
			if action != tt.wantAction || id != tt.wantID || ok != tt.wantOK {
				t.Errorf("parseCallback(%q) = (%q, %d, %v), want (%q, %d, %v)", tt.data, action, id, ok, tt.wantAction, tt.wantID, tt.wantOK)
			}
		})
	}
}

func TestIsTerminal(t *testing.T) {
	tests := map[string]bool{
		"completed": true,
		"failed":    true,
		"running":   false,
		"":          false,
	}
	for status, want := range tests {
		if got := isTerminal(status); got != want {
			t.Errorf("isTerminal(%q) = %v, want %v", status, got, want)
		}
	}
}

func TestRunReply(t *testing.T) {
	tests := []struct {
		name string
		run  map[string]any
		want string
	}{
		{name: "completed", run: map[string]any{"status": "completed", "stdout": "hello"}, want: "hello"},
		{name: "blank output", run: map[string]any{"status": "completed", "stdout": "  "}, want: "(done, no output)"},
		{name: "failed", run: map[string]any{"status": "failed", "stderr": "boom"}, want: "boom"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := runReply(tt.run); !strings.Contains(got, tt.want) {
				t.Errorf("runReply(%v) = %q, want it to contain %q", tt.run, got, tt.want)
			}
		})
	}
}

func TestFormatBudget(t *testing.T) {
	paused := formatBudget(map[string]any{"paused": true, "reason": "cap"})
	if !strings.Contains(paused, "PAUSED") || !strings.Contains(paused, "cap") {
		t.Errorf("formatBudget(paused) = %q, want PAUSED and cap", paused)
	}

	active := formatBudget(map[string]any{"paused": false})
	if !strings.Contains(active, "active") {
		t.Errorf("formatBudget(active) = %q, want active", active)
	}
}

func TestPollTurnAndReply(t *testing.T) {
	dc := &recordingDaemon{run: map[string]any{"status": "completed", "stdout": "done"}}
	if got := pollTurnAndReply(dc, 1, time.Microsecond, 3); got != "done" {
		t.Errorf("pollTurnAndReply() = %q, want %q", got, "done")
	}
}

func TestHandleMessageKillOn(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{}

	HandleMessage(bot, dc, NewTracker(), 42, "/kill on")

	if len(dc.setKillCalls) != 1 || !dc.setKillCalls[0] {
		t.Fatalf("SetKill calls = %v, want [true]", dc.setKillCalls)
	}
	if len(bot.messages) != 1 || !strings.Contains(bot.messages[0].text, "ENGAGED") {
		t.Errorf("messages = %v, want one containing ENGAGED", bot.messages)
	}
}

func TestHandleMessageHelp(t *testing.T) {
	bot := &recordingBot{}
	HandleMessage(bot, &recordingDaemon{}, NewTracker(), 42, "/help")

	if len(bot.messages) != 1 || bot.messages[0].text == "" {
		t.Errorf("messages = %v, want non-empty help text", bot.messages)
	}
}

func TestProjShortcutIsDeterministicNoLLM(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{projects: []map[string]any{
		{"project_id": "a", "mode": "off", "project_root": nil, "pending": float64(2)},
		{"project_id": "b", "mode": "shadow", "project_root": `C:\x`, "pending": float64(0)},
	}}

	HandleMessage(bot, dc, NewTracker(), 42, "/projects")

	if dc.getProjectsCalls != 1 {
		t.Errorf("GetProjects calls = %d, want 1", dc.getProjectsCalls)
	}
	if dc.sendAssistantCalls != 0 {
		t.Errorf("SendAssistantMessage calls = %d, want 0", dc.sendAssistantCalls)
	}
	if len(bot.htmlMessages) != 1 {
		t.Fatalf("SendHTML calls = %d, want 1", len(bot.htmlMessages))
	}
	if got := bot.htmlMessages[0].text; !strings.Contains(got, "<b>a</b>") || !strings.Contains(got, "<b>b</b>") {
		t.Errorf("SendHTML text = %q, want project ids a and b", got)
	}
}

func TestProjFilterMatchesOne(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{projects: []map[string]any{
		{"project_id": "a", "mode": "off", "project_root": nil, "pending": float64(2)},
		{"project_id": "b", "mode": "shadow", "project_root": `C:\x`, "pending": float64(0)},
	}}

	HandleMessage(bot, dc, NewTracker(), 42, "/proj b")

	if dc.sendAssistantCalls != 0 {
		t.Errorf("SendAssistantMessage calls = %d, want 0", dc.sendAssistantCalls)
	}
	if len(bot.htmlMessages) != 1 {
		t.Fatalf("SendHTML calls = %d, want 1", len(bot.htmlMessages))
	}
	if got := bot.htmlMessages[0].text; !strings.Contains(got, "<b>b</b>") || strings.Contains(got, "<b>a</b>") {
		t.Errorf("SendHTML text = %q, want only project id b", got)
	}
}

func TestHandleMessageCancelTrackedTurn(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{}
	tracker := NewTracker()
	tracker.Set(42, 7)

	HandleMessage(bot, dc, tracker, 42, "/cancel")

	if len(dc.cancelCalls) != 1 || dc.cancelCalls[0] != 7 {
		t.Fatalf("CancelRun calls = %v, want [7]", dc.cancelCalls)
	}
	if len(bot.messages) != 1 || bot.messages[0].text != "cancelled turn 7" {
		t.Errorf("messages = %v, want cancelled turn 7", bot.messages)
	}
}

func TestHandleCallbackApprove(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{}
	cb := telegram.CallbackQuery{
		ID:      "q",
		Data:    "approve:5",
		Message: &telegram.Message{Chat: telegram.Chat{ID: 99}},
	}

	HandleCallback(bot, dc, cb)

	if len(dc.approveCalls) != 1 || dc.approveCalls[0] != 5 {
		t.Fatalf("ApproveProposal calls = %v, want [5]", dc.approveCalls)
	}
	if len(bot.answers) != 1 || bot.answers[0].id != "q" {
		t.Errorf("callback answers = %v, want one for q", bot.answers)
	}
	if len(bot.messages) != 1 || !strings.Contains(bot.messages[0].text, "approved") {
		t.Errorf("messages = %v, want one containing approved", bot.messages)
	}
}

func TestStartTurnError(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{sendAssistantErr: errors.New("busy")}

	startTurn(bot, dc, NewTracker(), 42, "hello")

	if len(bot.messages) != 1 || !strings.Contains(bot.messages[0].text, "couldn't start turn") {
		t.Errorf("messages = %v, want couldn't start turn", bot.messages)
	}
}

func TestSendReplyUsesHTMLAndChunks(t *testing.T) {
	bot := &recordingBot{}

	sendReply(bot, 42, "**bold**")

	if len(bot.htmlMessages) == 0 {
		t.Fatal("SendHTML calls = 0, want at least one")
	}
	if !strings.Contains(bot.htmlMessages[0].text, "<b>bold</b>") {
		t.Errorf("SendHTML text = %q, want converted bold tag", bot.htmlMessages[0].text)
	}
}

func TestSendReplyFallsBackToPlaintextOnHTMLError(t *testing.T) {
	bot := &recordingBot{htmlErr: errors.New("invalid entities")}

	sendReply(bot, 42, "**bold**")

	if len(bot.messages) != 1 {
		t.Fatalf("SendMessage calls = %d, want 1", len(bot.messages))
	}
	if got := bot.messages[0].text; got != "bold" || strings.ContainsAny(got, "<>") {
		t.Errorf("SendMessage text = %q, want tag-free bold", got)
	}
}

func TestBuildAttachmentPrompt(t *testing.T) {
	prompt, ok := buildAttachmentPrompt("C:/tmp/document.pdf", "C:/tmp/photo.jpg")
	if !ok || prompt != "The user sent a document. File saved at: C:/tmp/document.pdf" {
		t.Errorf("buildAttachmentPrompt(document, photo) = (%q, %v), want document prompt", prompt, ok)
	}

	prompt, ok = buildAttachmentPrompt("", "")
	if ok || prompt != "" {
		t.Errorf("buildAttachmentPrompt(empty, empty) = (%q, %v), want (empty, false)", prompt, ok)
	}
}

func TestResolveIncomingVoiceWithoutTranscriber(t *testing.T) {
	dl := fakeDownloader{remotePath: "voice/file.ogg", data: []byte("voice")}
	msg := &telegram.Message{Voice: &telegram.Voice{FileID: "v1"}}

	if got := resolveIncoming(dl, "", msg); !strings.Contains(got, "unavailable") {
		t.Errorf("resolveIncoming(voice) = %q, want unavailable note", got)
	}
}

func TestResolveIncomingDocument(t *testing.T) {
	dl := fakeDownloader{remotePath: "documents/notes.pdf", data: []byte("document")}
	msg := &telegram.Message{Document: &telegram.Document{FileID: "d1", FileName: "notes.pdf"}}

	got := resolveIncoming(dl, "", msg)
	if !strings.Contains(got, "File saved at:") || !strings.HasSuffix(got, ".pdf") {
		t.Errorf("resolveIncoming(document) = %q, want saved .pdf prompt", got)
	}
}

func TestResolveIncomingPlainText(t *testing.T) {
	msg := &telegram.Message{Text: "hello"}

	if got := resolveIncoming(fakeDownloader{}, "", msg); got != "hello" {
		t.Errorf("resolveIncoming(text) = %q, want %q", got, "hello")
	}
}

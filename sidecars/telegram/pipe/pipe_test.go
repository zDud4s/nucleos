package pipe

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"nucleostelegram/config"
	"nucleostelegram/daemon"
	"nucleostelegram/notifier"
	"nucleostelegram/telegram"
)

func TestHelperProcess(t *testing.T) {
	if os.Getenv("GO_WANT_HELPER_PROCESS") != "1" {
		return
	}

	args := os.Args
	for i, arg := range args {
		if arg == "--" {
			args = args[i+1:]
			break
		}
	}

	switch os.Getenv("HELPER_MODE") {
	case "echo":
		fmt.Println(strings.Join(args, " "))
	case "sleep":
		time.Sleep(30 * time.Second)
	case "print":
		fmt.Println(os.Getenv("HELPER_TEXT"))
	}
	os.Exit(0)
}

func helperCommand(t *testing.T, mode string) string {
	t.Helper()

	dir := t.TempDir()
	name := "helper" + filepath.Ext(os.Args[0])
	helper := filepath.Join(dir, name)
	bytes, err := os.ReadFile(os.Args[0])
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(helper, bytes, 0o755); err != nil {
		t.Fatal(err)
	}
	t.Chdir(dir)
	t.Setenv("GO_WANT_HELPER_PROCESS", "1")
	t.Setenv("HELPER_MODE", mode)
	return "." + string(filepath.Separator) + name + " -test.run=^TestHelperProcess$ --"
}

type sentMessage struct {
	to   telegram.Destination
	text string
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
	// buttonErrOnce fails the first button send only, standing in for a send that is lost while
	// the connection is down and works again on the next attempt.
	buttonErrOnce error
	buttonCalls   int
	// sendErrFor, when set, decides what a plain send to a given destination returns, after the
	// attempt has been recorded — so a test can refuse a topic and accept the bare chat.
	sendErrFor func(telegram.Destination) error
}

func (b *recordingBot) SendMessage(to telegram.Destination, text string) error {
	b.messages = append(b.messages, sentMessage{to: to, text: text})
	if b.sendErrFor != nil {
		return b.sendErrFor(to)
	}
	return nil
}

func (b *recordingBot) SendHTML(to telegram.Destination, html string) error {
	b.htmlMessages = append(b.htmlMessages, sentMessage{to: to, text: html})
	return b.htmlErr
}

func (b *recordingBot) SendMessageWithButtons(to telegram.Destination, text string, _ [][]telegram.Button) error {
	b.buttonCalls++
	b.messages = append(b.messages, sentMessage{to: to, text: text})
	if b.buttonCalls == 1 {
		return b.buttonErrOnce
	}
	return nil
}

func (b *recordingBot) AnswerCallbackQuery(callbackID, text string) error {
	b.answers = append(b.answers, callbackAnswer{id: callbackID, text: text})
	return nil
}

type recordingDaemon struct {
	sendAssistantErr   error
	sendAssistantCalls int
	// noteCalls records the text of every CreateNote call; noteErr is what the next one reports.
	noteCalls []string
	noteErr   error
	// answerCalls records every AnswerCapture call; answerErr is what the next one reports and
	// answerReleased what it says about the request.
	answerCalls    []answerCall
	answerErr      error
	answerReleased bool
	// The notification policy this fake daemon serves, and the error it serves instead. The zero
	// value is an empty policy, which allows everything — so every test written before the policy
	// existed keeps the behaviour it was written against.
	notifyPolicy    notifier.Policy
	notifyPolicyErr error
	// refused is what the injection barrier turned away and nobody has read yet.
	refused []map[string]any
	// lastChatID is the key the daemon was told to route on, which is the string a conversation is
	// registered under — so a test can prove the topic survived the trip.
	lastChatID string
	run        map[string]any
	runErr     error
	// getRunFailures is how many leading GetRun calls report the daemon as unreachable, for the
	// tests that pin what happens to a turn that outlives a daemon restart.
	getRunFailures   int
	getRunCalls      int
	projects         []map[string]any
	projectsErr      error
	getProjectsCalls int
	setKillCalls     []bool
	cancelCalls      []int64
	approveCalls     []int64
	triageCalls      int
	emailQueue       []map[string]any
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

func (d *recordingDaemon) SendAssistantMessage(chatID, _ string) (int64, error) {
	d.sendAssistantCalls++
	d.lastChatID = chatID
	return 0, d.sendAssistantErr
}

type answerCall struct {
	jobID int64
	text  string
}

func (d *recordingDaemon) AnswerCapture(jobID int64, text string) (int64, bool, error) {
	d.answerCalls = append(d.answerCalls, answerCall{jobID, text})
	if d.answerErr != nil {
		return 0, false, d.answerErr
	}
	return 31, d.answerReleased, nil
}

func (d *recordingDaemon) CreateNote(text string) (int64, error) {
	d.noteCalls = append(d.noteCalls, text)
	if d.noteErr != nil {
		return 0, d.noteErr
	}
	return 17, nil
}

func (d *recordingDaemon) GetRun(int64) (map[string]any, error) {
	d.getRunCalls++
	if d.getRunCalls <= d.getRunFailures {
		return nil, errors.New("daemon unreachable")
	}
	return d.run, d.runErr
}

func (d *recordingDaemon) GetProposals() ([]map[string]any, error) {
	return nil, nil
}

func (d *recordingDaemon) GetRefusedActions() ([]map[string]any, error) {
	return d.refused, nil
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

func (d *recordingDaemon) GetNotifyPolicy() (notifier.Policy, error) {
	return d.notifyPolicy, d.notifyPolicyErr
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

	HandleMessage(bot, dc, NewTracker(), telegram.Destination{ChatID: 42}, "/kill on")

	if len(dc.setKillCalls) != 1 || !dc.setKillCalls[0] {
		t.Fatalf("SetKill calls = %v, want [true]", dc.setKillCalls)
	}
	if len(bot.messages) != 1 || !strings.Contains(bot.messages[0].text, "ENGAGED") {
		t.Errorf("messages = %v, want one containing ENGAGED", bot.messages)
	}
}

func TestHandleMessageHelp(t *testing.T) {
	bot := &recordingBot{}
	HandleMessage(bot, &recordingDaemon{}, NewTracker(), telegram.Destination{ChatID: 42}, "/help")

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

	HandleMessage(bot, dc, NewTracker(), telegram.Destination{ChatID: 42}, "/projects")

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

	HandleMessage(bot, dc, NewTracker(), telegram.Destination{ChatID: 42}, "/proj b")

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
	tracker.Set("42", 7)

	HandleMessage(bot, dc, tracker, telegram.Destination{ChatID: 42}, "/cancel")

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

	startTurn(bot, dc, NewTracker(), telegram.Destination{ChatID: 42}, "hello")

	if len(bot.messages) != 1 || !strings.Contains(bot.messages[0].text, "couldn't start turn") {
		t.Errorf("messages = %v, want couldn't start turn", bot.messages)
	}
}

func TestSendReplyUsesHTMLAndChunks(t *testing.T) {
	bot := &recordingBot{}

	sendReply(bot, telegram.Destination{ChatID: 42}, "**bold**")

	if len(bot.htmlMessages) == 0 {
		t.Fatal("SendHTML calls = 0, want at least one")
	}
	if !strings.Contains(bot.htmlMessages[0].text, "<b>bold</b>") {
		t.Errorf("SendHTML text = %q, want converted bold tag", bot.htmlMessages[0].text)
	}
}

func TestSendReplyFallsBackToPlaintextOnHTMLError(t *testing.T) {
	bot := &recordingBot{htmlErr: errors.New("invalid entities")}

	sendReply(bot, telegram.Destination{ChatID: 42}, "**bold**")

	if len(bot.messages) != 1 {
		t.Fatalf("SendMessage calls = %d, want 1", len(bot.messages))
	}
	if got := bot.messages[0].text; got != "bold" || strings.ContainsAny(got, "<>") {
		t.Errorf("SendMessage text = %q, want tag-free bold", got)
	}
}

func TestBuildAttachmentPrompt(t *testing.T) {
	prompt, ok := buildAttachmentPrompt("C:/tmp/document.pdf", "C:/tmp/photo.jpg", "")
	if !ok || prompt != "The user sent a document. File saved at: C:/tmp/document.pdf" {
		t.Errorf("buildAttachmentPrompt(document, photo) = (%q, %v), want document prompt", prompt, ok)
	}

	prompt, ok = buildAttachmentPrompt("", "", "")
	if ok || prompt != "" {
		t.Errorf("buildAttachmentPrompt(empty, empty) = (%q, %v), want (empty, false)", prompt, ok)
	}
}

func TestResolveIncomingVoiceWithoutTranscriber(t *testing.T) {
	dl := fakeDownloader{remotePath: "voice/file.ogg", data: []byte("voice")}
	msg := &telegram.Message{Voice: &telegram.Voice{FileID: "v1"}}

	if got := resolveIncoming(dl, nil, "", msg); !strings.Contains(got, "unavailable") {
		t.Errorf("resolveIncoming(voice) = %q, want unavailable note", got)
	}
}

func TestResolveIncomingDocument(t *testing.T) {
	dl := fakeDownloader{remotePath: "documents/notes.pdf", data: []byte("document")}
	msg := &telegram.Message{Document: &telegram.Document{FileID: "d1", FileName: "notes.pdf"}}

	got := resolveIncoming(dl, nil, "", msg)
	if !strings.Contains(got, "File saved at:") || !strings.HasSuffix(got, ".pdf") {
		t.Errorf("resolveIncoming(document) = %q, want saved .pdf prompt", got)
	}
}

func TestResolveIncomingPlainText(t *testing.T) {
	msg := &telegram.Message{Text: "hello"}

	if got := resolveIncoming(fakeDownloader{}, nil, "", msg); got != "hello" {
		t.Errorf("resolveIncoming(text) = %q, want %q", got, "hello")
	}
}

// A group chat id authorises a room, not a person. Every member — including whoever is added next
// month — would otherwise be able to disarm the kill switch, so in a group the sender is checked
// against an explicit allowlist and nothing else.
func TestOnlyAnAuthorisedSenderReachesTheDaemon(t *testing.T) {
	cfg := config.Config{AllowedChatID: -100200300, AllowedUserIDs: []int64{7}}
	group := telegram.Chat{ID: -100200300}

	stranger := &recordingDaemon{}
	HandleUpdate(&recordingBot{}, stranger, fakeDownloader{}, cfg, NewTracker(), telegram.Update{
		Message: &telegram.Message{Chat: group, From: &telegram.User{ID: 8}, Text: "/kill off"},
	})
	if len(stranger.setKillCalls) != 0 {
		t.Errorf("SetKill calls from an unlisted member = %v, want none", stranger.setKillCalls)
	}

	owner := &recordingDaemon{}
	HandleUpdate(&recordingBot{}, owner, fakeDownloader{}, cfg, NewTracker(), telegram.Update{
		Message: &telegram.Message{Chat: group, From: &telegram.User{ID: 7}, Text: "/kill off"},
	})
	if len(owner.setKillCalls) != 1 {
		t.Errorf("SetKill calls from the allowed member = %v, want one", owner.setKillCalls)
	}
}

// The inline buttons were checked against the chat the button lives in, never against the finger
// that pressed it — so in a group anyone could approve an agent's proposal by tapping.
func TestAnUnauthorisedPressCannotApproveAProposal(t *testing.T) {
	cfg := config.Config{AllowedChatID: -100200300, AllowedUserIDs: []int64{7}}
	bot := &recordingBot{}
	dc := &recordingDaemon{}

	HandleUpdate(bot, dc, fakeDownloader{}, cfg, NewTracker(), telegram.Update{
		CallbackQuery: &telegram.CallbackQuery{
			ID:      "q",
			Data:    "approve:5",
			From:    telegram.User{ID: 8},
			Message: &telegram.Message{Chat: telegram.Chat{ID: -100200300}},
		},
	})

	if len(dc.approveCalls) != 0 {
		t.Fatalf("ApproveProposal calls = %v, want none", dc.approveCalls)
	}
	if len(bot.answers) != 1 || bot.answers[0].text == "" {
		t.Errorf("callback answers = %v, want the press answered rather than ignored", bot.answers)
	}
}

// A voice note is a recording of someone's voice sitting unencrypted in the system temp directory.
// It is needed for exactly as long as the transcription takes.
func TestAVoiceRecordingDoesNotOutliveItsTranscription(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("TMP", dir)
	t.Setenv("TMPDIR", dir)
	t.Setenv("TEMP", dir)

	dl := fakeDownloader{remotePath: "voice/file.ogg", data: []byte("voice")}
	resolveIncoming(dl, nil, "", &telegram.Message{Voice: &telegram.Voice{FileID: "v1"}})

	left, err := filepath.Glob(filepath.Join(dir, tempFilePattern))
	if err != nil {
		t.Fatal(err)
	}
	if len(left) != 0 {
		t.Errorf("temp files left behind = %v, want none", left)
	}
}

// Attachments have to outlive the turn that reads them, so they cannot be deleted on the spot — but
// "until someone notices" is not a retention policy for files from a private control channel.
func TestOldAttachmentsAreSweptFromTheTempDirectory(t *testing.T) {
	dir := t.TempDir()
	old := filepath.Join(dir, "nucleos-tg-123.jpg")
	fresh := filepath.Join(dir, "nucleos-tg-456.jpg")
	unrelated := filepath.Join(dir, "someone-elses-file.jpg")
	for _, path := range []string{old, fresh, unrelated} {
		if err := os.WriteFile(path, []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	stale := time.Now().Add(-48 * time.Hour)
	if err := os.Chtimes(old, stale, stale); err != nil {
		t.Fatal(err)
	}

	sweepTempFiles(dir, 24*time.Hour)

	if _, err := os.Stat(old); !os.IsNotExist(err) {
		t.Errorf("stat(old attachment) = %v, want it gone", err)
	}
	if _, err := os.Stat(fresh); err != nil {
		t.Errorf("stat(fresh attachment) = %v, want it kept for the turn still reading it", err)
	}
	if _, err := os.Stat(unrelated); err != nil {
		t.Errorf("stat(unrelated file) = %v, want files this sidecar did not create left alone", err)
	}
}

// A photo sent with an instruction in its caption used to reach the orchestrator as "The user sent
// a photo", instruction discarded — a turn spent on a file with no idea what to do with it.
func TestACaptionOnAnAttachmentReachesTheOrchestrator(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("TMP", dir)
	t.Setenv("TMPDIR", dir)
	dl := fakeDownloader{remotePath: "photos/x.jpg", data: []byte("photo")}
	msg := &telegram.Message{
		Photo:   []telegram.PhotoSize{{FileID: "p1"}},
		Caption: "what is the error in this screenshot?",
	}

	got := resolveIncoming(dl, nil, "", msg)
	if !strings.Contains(got, "what is the error in this screenshot?") {
		t.Errorf("resolveIncoming(captioned photo) = %q, want the caption carried through", got)
	}
	if !strings.Contains(got, "File saved at:") {
		t.Errorf("resolveIncoming(captioned photo) = %q, want the file path too", got)
	}
}

// A sticker, a location, a poll: none of them is text, and each used to start a turn with an empty
// prompt. Spending a run on nothing is worse than saying nothing can be done with it.
func TestAMessageWithNothingToActOnDoesNotStartATurn(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{}
	cfg := config.Config{AllowedChatID: 42}

	HandleUpdate(bot, dc, fakeDownloader{}, cfg, NewTracker(), telegram.Update{
		Message: &telegram.Message{Chat: telegram.Chat{ID: 42}, From: &telegram.User{ID: 42}},
	})

	if dc.sendAssistantCalls != 0 {
		t.Fatalf("SendAssistantMessage calls = %d, want none for a message with no content", dc.sendAssistantCalls)
	}
	if len(bot.messages) != 1 {
		t.Errorf("messages = %v, want one saying it cannot be read", bot.messages)
	}
}

// A voice note is a speech model's guess. It must not be able to fire a command silently — but the
// refusal has to be said out loud, or a person is left thinking the kill switch is off.
func TestASpokenCommandIsRefusedOutLoudInsteadOfExecuted(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("TMP", dir)
	t.Setenv("TMPDIR", dir)

	bot := &recordingBot{}
	dc := &recordingDaemon{}
	t.Setenv("HELPER_TEXT", "/kill off")
	cfg := config.Config{AllowedChatID: 42, TranscribeCmd: helperCommand(t, "print")}

	HandleUpdate(bot, dc, transcribingDownloader{}, cfg, NewTracker(), telegram.Update{
		Message: &telegram.Message{
			Chat:  telegram.Chat{ID: 42},
			From:  &telegram.User{ID: 42},
			Voice: &telegram.Voice{FileID: "v1"},
		},
	})

	if len(dc.setKillCalls) != 0 {
		t.Fatalf("SetKill calls from a voice note = %v, want none", dc.setKillCalls)
	}
	if len(bot.messages) != 1 || !strings.Contains(bot.messages[0].text, "/kill off") {
		t.Errorf("messages = %v, want one telling the user to type the command", bot.messages)
	}
}

// A turn outlives a daemon restart: the run keeps going server-side, and giving up on the first
// GetRun error threw away output that was still coming.
func TestPollingSurvivesADaemonThatBlinks(t *testing.T) {
	dc := &recordingDaemon{
		getRunFailures: 3,
		run:            map[string]any{"status": "completed", "stdout": "done"},
	}

	if got := pollTurnAndReply(dc, 1, time.Microsecond, 20); got != "done" {
		t.Errorf("pollTurnAndReply() = %q, want the output that arrived after the outage", got)
	}
}

// It still has to give up eventually: a daemon that is gone for good must produce an answer, not
// ten minutes of silence.
func TestPollingGivesUpWhenTheDaemonStaysDown(t *testing.T) {
	dc := &recordingDaemon{getRunFailures: 1000}

	got := pollTurnAndReply(dc, 1, time.Microsecond, 100)
	if !strings.Contains(got, "daemon unreachable") {
		t.Errorf("pollTurnAndReply() = %q, want the daemon error reported", got)
	}
	if dc.getRunCalls > 20 {
		t.Errorf("GetRun calls = %d, want it to stop asking long before the attempt limit", dc.getRunCalls)
	}
}

// The fallback fired a second send the instant the first failed. When the first failure is a
// throttle, that is the sidecar doubling its own traffic at the exact moment Telegram asked it to
// stop — the content was never the problem, so re-sending it as plain text cannot help.
func TestAThrottledReplyIsNotImmediatelyResentAsPlainText(t *testing.T) {
	bot := &recordingBot{htmlErr: &telegram.APIError{
		Method:     "sendMessage",
		StatusCode: 429,
		RetryAfter: 5 * time.Second,
	}}

	sendReply(bot, telegram.Destination{ChatID: 42}, "**bold**")

	if len(bot.messages) != 0 {
		t.Errorf("plain-text sends = %v, want none while throttled", bot.messages)
	}
}

// At boot the daemon is usually not up yet. Seeding was skipped when it failed, so the first
// successful poll treated the entire backlog as new: every pending proposal announced at once, each
// with a live Approve button under it.
func TestTheNotifierNeverAnnouncesTheBacklogItFindsAtBoot(t *testing.T) {
	bot := &recordingBot{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	dc := &bootingDaemon{
		recordingDaemon: &recordingDaemon{},
		unreachableFor:  3,
		stopAfter:       8,
		stop:            cancel,
		proposals: []map[string]any{
			{"id": float64(1), "tool_name": "shell"},
			{"id": float64(2), "tool_name": "shell"},
		},
		feed: []map[string]any{{"id": float64(9), "kind": "run", "summary": "old news"}},
	}

	RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: 42}, nil, time.Millisecond)

	if len(bot.messages) != 0 {
		t.Errorf("announcements = %v, want none for what was already there when the daemon came up", bot.messages)
	}
}

// The counterpart: once seeded, what actually happens next still has to arrive.
func TestTheNotifierAnnouncesWhatArrivesAfterItIsSeeded(t *testing.T) {
	bot := &recordingBot{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	dc := &bootingDaemon{
		recordingDaemon: &recordingDaemon{},
		unreachableFor:  1,
		stopAfter:       6,
		stop:            cancel,
		proposals:       []map[string]any{{"id": float64(1), "tool_name": "shell"}},
		arriving:        map[int][]map[string]any{4: {{"id": float64(1)}, {"id": float64(2), "tool_name": "git push"}}},
	}

	RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: 42}, nil, time.Millisecond)

	if len(bot.messages) != 1 || !strings.Contains(bot.messages[0].text, "git push") {
		t.Errorf("announcements = %v, want exactly the proposal that arrived after seeding", bot.messages)
	}
}

// Every feed line goes to the configured chat, whatever else it carries.
//
// A row that still has an `errand_id` field must not be routed, skipped or dropped by it: the
// field means nothing to this sidecar any more.
func TestEveryFeedLineGoesToTheConfiguredChat(t *testing.T) {
	bot := &recordingBot{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	configured := telegram.Destination{ChatID: 42}
	dc := &bootingDaemon{
		recordingDaemon: &recordingDaemon{},
		stopAfter:       6,
		stop:            cancel,
		feedArriving: map[int][]map[string]any{3: {
			{"id": float64(31), "errand_id": float64(3), "kind": "errand_rule_fired",
				"summary": "first line arrived"},
			{"id": float64(32), "errand_id": nil, "kind": "kill_switch",
				"summary": "second line arrived"},
		}},
	}

	RunNotifier(ctx, bot, dc, configured, nil, time.Millisecond)

	for _, needle := range []string{"first line", "second line"} {
		found := false
		for _, message := range bot.messages {
			if strings.Contains(message.text, needle) {
				found = true
				if message.to != configured {
					t.Errorf("%q went to %+v, want the configured chat %+v", needle, message.to, configured)
				}
			}
		}
		if !found {
			t.Errorf("%q was never sent: %v", needle, bot.messages)
		}
	}
}

// A proposal was marked as announced before anyone had been announced to, so a send that failed
// took the prompt with it: the agent's action sits waiting and the person who has to decide never
// learns there is anything to decide.
func TestAProposalAnnouncementThatFailedToSendIsOfferedAgain(t *testing.T) {
	bot := &recordingBot{buttonErrOnce: errors.New("connection reset")}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	waiting := []map[string]any{{"id": float64(5), "tool_name": "git push"}}
	dc := &bootingDaemon{
		recordingDaemon: &recordingDaemon{},
		stopAfter:       6,
		stop:            cancel,
		arriving:        map[int][]map[string]any{3: waiting, 4: waiting, 5: waiting},
	}

	RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: 42}, nil, time.Millisecond)

	if bot.buttonCalls != 2 {
		t.Fatalf("proposal announcements attempted = %d, want 2 (the lost one and its retry)", bot.buttonCalls)
	}
	for i, message := range bot.messages {
		if !strings.Contains(message.text, "git push") {
			t.Errorf("announcement %d = %q, want the proposal that is still waiting", i, message.text)
		}
	}
}

// transcribingDownloader stands in for a real download so the transcriber has a file to be handed.
type transcribingDownloader struct{}

func (transcribingDownloader) GetFile(string) (string, error) { return "voice/note.ogg", nil }

func (transcribingDownloader) DownloadFile(string) ([]byte, error) { return []byte("audio"), nil }

// bootingDaemon is a daemon that is not up yet, which is the normal state of affairs when this
// sidecar starts. It cancels the notifier's context itself once it has been polled enough times, so
// the test never has to read the bot's records while the notifier is still writing them.
type bootingDaemon struct {
	*recordingDaemon
	unreachableFor int
	stopAfter      int
	stop           context.CancelFunc
	calls          int
	proposals      []map[string]any
	feed           []map[string]any
	arriving       map[int][]map[string]any
	// feedArriving is `arriving` for the feed: what shows up on a given poll rather than what was
	// already there at boot. Seeding marks everything present at boot as told, so a line placed in
	// `feed` can never be announced — which is correct, and is why a test about announcing needs
	// this instead.
	feedArriving map[int][]map[string]any
	// policyFrom is the notification policy served from a given poll onwards — the latest entry at
	// or before the current poll wins. A test needs this to silence a family, watch a line go by,
	// and then turn the family back on, which is the ordering guarantee the filter's position
	// after NewFeedItems exists to give.
	policyFrom map[int]notifier.Policy
	// killFrom and budgetPausedFrom are the polls from which the kill switch reads as engaged and
	// the budget as paused. Both exist so a test can prove governance still speaks while the
	// policy silences everything the feed carries.
	killFrom         int
	budgetPausedFrom int
}

func (d *bootingDaemon) GetNotifyPolicy() (notifier.Policy, error) {
	if d.notifyPolicyErr != nil {
		return notifier.Policy{}, d.notifyPolicyErr
	}
	latest, at := d.notifyPolicy, -1
	for poll, policy := range d.policyFrom {
		if d.calls >= poll && poll > at {
			latest, at = policy, poll
		}
	}
	return latest, nil
}

func (d *bootingDaemon) GetKill() (bool, error) {
	return d.killFrom > 0 && d.calls >= d.killFrom, nil
}

func (d *bootingDaemon) GetBudget() (map[string]any, error) {
	if d.budgetPausedFrom > 0 && d.calls >= d.budgetPausedFrom {
		return map[string]any{"paused": true, "reason": "the ceiling was reached"}, nil
	}
	return map[string]any{}, nil
}

func (d *bootingDaemon) GetProposals() ([]map[string]any, error) {
	d.calls++
	if d.calls >= d.stopAfter {
		d.stop()
	}
	if d.calls <= d.unreachableFor {
		return nil, errors.New("daemon not up yet")
	}
	if arrived, ok := d.arriving[d.calls]; ok {
		return arrived, nil
	}
	return d.proposals, nil
}

func (d *bootingDaemon) GetFeed() ([]map[string]any, error) {
	if d.calls <= d.unreachableFor {
		return nil, errors.New("daemon not up yet")
	}
	if arrived, ok := d.feedArriving[d.calls]; ok {
		return arrived, nil
	}
	return d.feed, nil
}

// A daemon whose voice pillar can be scripted, plus the Daemon methods the pipe never reaches on this
// path. Only `VoiceCapture` is exercised; the rest exist so the value satisfies `Daemon`.
type voiceDaemon struct {
	Daemon
	text       string
	configured bool
	err        error
	gotFormat  string
	gotMillis  int64
	gotBytes   int
}

func (v *voiceDaemon) VoiceCapture(audio []byte, format string, durationMs int64) (string, bool, error) {
	v.gotFormat = format
	v.gotMillis = durationMs
	v.gotBytes = len(audio)
	return v.text, v.configured, v.err
}

// The point of the whole change: when the núcleo can transcribe, it does — hints and cleanup included —
// and this process does not run a transcriber of its own.
func TestVoiceGoesThroughTheDaemonWhenThePillarIsArmed(t *testing.T) {
	dl := fakeDownloader{data: []byte("OggS fake opus")}
	vd := &voiceDaemon{text: "the núcleo owns the DB.", configured: true}
	msg := &telegram.Message{Voice: &telegram.Voice{FileID: "v1", Duration: 4}}

	// `transcribeCmd` is deliberately a command that would FAIL if it ran, so a passing test proves the
	// local path was not taken rather than merely that the answer was right.
	got := resolveIncoming(dl, vd, "definitely-not-a-program", msg)

	if got != "the núcleo owns the DB." {
		t.Errorf("resolveIncoming(voice) = %q, want the daemon's transcript", got)
	}
	if vd.gotFormat != "ogg" {
		t.Errorf("format = %q, want ogg — Telegram sends Opus and the daemon names its temp file from this", vd.gotFormat)
	}
	if vd.gotMillis != 4000 {
		t.Errorf("durationMs = %d, want 4000 (Telegram reports seconds)", vd.gotMillis)
	}
	if vd.gotBytes == 0 {
		t.Error("no audio reached the daemon")
	}
}

// The regression this guards: a voice note must keep arriving for someone who never configured the
// voice pillar. `configured: false` is the daemon saying 503, or not answering at all.
func TestVoiceFallsBackToTheLocalCommandWhenThePillarIsOff(t *testing.T) {
	dl := fakeDownloader{data: []byte("OggS fake opus")}
	vd := &voiceDaemon{configured: false}
	msg := &telegram.Message{Voice: &telegram.Voice{FileID: "v1", Duration: 2}}

	// An empty command makes the local transcriber report ErrNoTranscriber, so reaching the
	// "unavailable" note proves the fallback ran instead of the answer being taken from the daemon.
	got := resolveIncoming(dl, vd, "", msg)

	if !strings.Contains(got, "unavailable") {
		t.Errorf("resolveIncoming(voice) = %q, want the local path to have been tried", got)
	}
}

// A daemon that CAN transcribe and failed is not a reason to try again locally: it already has the
// recording's one chance, and a second transcription would be a different answer with no way to tell
// which was right.
func TestAFailingDaemonIsReportedRatherThanRetriedLocally(t *testing.T) {
	dl := fakeDownloader{data: []byte("OggS fake opus")}
	vd := &voiceDaemon{configured: true, err: errors.New("transcriber exited with 1")}
	msg := &telegram.Message{Voice: &telegram.Voice{FileID: "v1", Duration: 2}}

	got := resolveIncoming(dl, vd, "", msg)

	if !strings.Contains(got, "unavailable") {
		t.Errorf("resolveIncoming(voice) = %q, want an unavailable note", got)
	}
}

// 204 from the daemon means it ran and heard nothing. Distinct from a failure, because the person
// should be told their recording was silent rather than that dictation is broken.
func TestSilenceFromTheDaemonSaysNothingWasHeard(t *testing.T) {
	dl := fakeDownloader{data: []byte("OggS fake opus")}
	vd := &voiceDaemon{configured: true, text: "   "}
	msg := &telegram.Message{Voice: &telegram.Voice{FileID: "v1", Duration: 2}}

	got := resolveIncoming(dl, vd, "", msg)

	if !strings.Contains(got, "nothing was heard") {
		t.Errorf("resolveIncoming(voice) = %q, want a nothing-was-heard note", got)
	}
}

func TestChatKey(t *testing.T) {
	for _, tc := range []struct {
		name string
		to   telegram.Destination
		want string
	}{
		{"a topic of a group", telegram.Destination{ChatID: -1001234567890, ThreadID: 7}, "-1001234567890:7"},
		// The guard on this whole change. A one-to-one chat, and a group's General, have to produce
		// the key they produce today — byte for byte. Anything else and every existing conversation
		// loses its session on the day this ships.
		{"no topic", telegram.Destination{ChatID: -1001234567890}, "-1001234567890"},
		{"a positive id", telegram.Destination{ChatID: 12345, ThreadID: 3}, "12345:3"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := ChatKey(tc.to); got != tc.want {
				t.Errorf("ChatKey(%+v) = %q, want %q", tc.to, got, tc.want)
			}
		})
	}
}

// The way back. `ChatKey` composes and, until now, nothing took one apart — which was fine while
// every key the sidecar held came from an update it had just received, and stops being fine the
// moment the daemon hands one back.
//
// Every case here is a case of `TestChatKey` read in the other direction, deliberately, because the
// property that matters is that the pair are inverses. A decomposition that disagreed with the
// composition would put answers in a topic nobody is reading.
func TestDestinationFromKey(t *testing.T) {
	for _, tc := range []struct {
		name string
		key  string
		want telegram.Destination
	}{
		{"a topic of a group", "-1001234567890:7", telegram.Destination{ChatID: -1001234567890, ThreadID: 7}},
		{"no topic", "-1001234567890", telegram.Destination{ChatID: -1001234567890}},
		{"a positive id", "12345:3", telegram.Destination{ChatID: 12345, ThreadID: 3}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, ok := DestinationFromKey(tc.key)
			if !ok {
				t.Fatalf("DestinationFromKey(%q) refused a key ChatKey produces", tc.key)
			}
			if got != tc.want {
				t.Errorf("DestinationFromKey(%q) = %+v, want %+v", tc.key, got, tc.want)
			}
			if round := ChatKey(got); round != tc.key {
				t.Errorf("ChatKey(DestinationFromKey(%q)) = %q; the two are not inverses", tc.key, round)
			}
		})
	}
}

// A key that cannot be read is refused, and never quietly becomes the configured chat.
//
// This is the one that matters for what gets SEEN. The fallback nobody writes on purpose is "send
// it to the usual place", and the usual place is a group's General: a message
// posted where its conversation is not. Refusing costs a log line and a missing notification;
// guessing costs the thing being in the wrong room, and nothing about it looks wrong afterwards.
func TestAKeyThatCannotBeReadIsRefusedRatherThanGuessedAt(t *testing.T) {
	for _, key := range []string{"", "abc", "-100:", ":7", "-100:abc", "-100:7:9", " -100"} {
		t.Run(key, func(t *testing.T) {
			if got, ok := DestinationFromKey(key); ok {
				t.Errorf("DestinationFromKey(%q) = %+v, true; want a refusal", key, got)
			}
		})
	}
}

// `/cancel` cancels the turn of the topic it was typed in, not the last turn of the group. The
// tracker is keyed on the same string the daemon routes on, so the two cannot disagree about what
// "this conversation" means.
func TestTheTrackerKeepsOneTurnPerTopic(t *testing.T) {
	tracker := NewTracker()
	tracker.Set("-100123:7", 11)
	tracker.Set("-100123:9", 22)

	if got, ok := tracker.Get("-100123:7"); !ok || got != 11 {
		t.Errorf("Get(topic 7) = %d, %v; want 11, true", got, ok)
	}
	if got, ok := tracker.Get("-100123:9"); !ok || got != 22 {
		t.Errorf("Get(topic 9) = %d, %v; want 22, true", got, ok)
	}
	if _, ok := tracker.Get("-100123"); ok {
		t.Error("the group itself had no turn and must not inherit one from a topic in it")
	}
}

// The whole point of Task 17, seen from the outside: a question asked in a topic is answered in
// that topic. Nothing errors when this is wrong — the reply simply appears in General.
func TestAReplyGoesBackToTheTopicItCameFrom(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{}
	cfg := config.Config{AllowedChatID: -100123, AllowedUserIDs: []int64{7}}

	HandleUpdate(bot, dc, fakeDownloader{}, cfg, NewTracker(), telegram.Update{
		Message: &telegram.Message{
			Chat:            telegram.Chat{ID: -100123},
			From:            &telegram.User{ID: 7},
			MessageThreadID: 7,
			IsTopicMessage:  true,
			Text:            "olá",
		},
	})

	if len(bot.messages) == 0 {
		t.Fatal("the bot said nothing at all")
	}
	for _, sent := range bot.messages {
		if sent.to != (telegram.Destination{ChatID: -100123, ThreadID: 7}) {
			t.Errorf("%q went to %+v, want the topic it came from", sent.text, sent.to)
		}
	}
	if dc.lastChatID != "-100123:7" {
		t.Errorf("the daemon was told chat %q, want the topic key", dc.lastChatID)
	}
}

// A proposal with no project must not claim one.
//
// `project_id` is always PRESENT in the JSON and null for anything that is not a project's — serde
// writes the field either way — and the old check tested presence, so every proposal without a
// project rendered "project: <nil>". Harmless-looking, and it is the line a person
// reads to decide what they are approving.
func TestAProposalWithoutAProjectDoesNotInventOne(t *testing.T) {
	got := formatProposal(map[string]any{
		"id":         float64(4),
		"tool_name":  "send_email",
		"reasoning":  "this turn has read third-party content and can no longer act",
		"project_id": nil,
	})

	if strings.Contains(got, "project") {
		t.Errorf("formatProposal = %q, want no project line for a proposal that has none", got)
	}
	if strings.Contains(got, "nil") || strings.Contains(got, "null") {
		t.Errorf("formatProposal = %q, want no rendered null", got)
	}
}

// The record has to reach the person, and the person is here.
//
// Listed under /proposals with the approvable ones because that is the command somebody already
// types, and separated from them by having no buttons: /approve and /reject answer 409 for anything
// that is not an action-approval, so a button here would be one that cannot work.
func TestProposalsAlsoShowsWhatTheBarrierRefused(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{refused: []map[string]any{{
		"id":        float64(9),
		"tool_name": "send_email",
		"reasoning": "this turn has read third-party content and can no longer act",
	}}}

	HandleMessage(bot, dc, NewTracker(), topic(-100123, 7), "/proposals")

	all := ""
	for _, m := range bot.messages {
		all += m.text + "\n"
	}
	if !strings.Contains(all, "send_email") {
		t.Errorf("messages = %q, want the refused action", all)
	}
	if bot.buttonCalls != 0 {
		t.Errorf("button sends = %d, want none: neither approve nor reject works on this", bot.buttonCalls)
	}
}

func lastMessage(t *testing.T, bot *recordingBot) string {
	t.Helper()
	if len(bot.messages) == 0 {
		t.Fatal("the bot said nothing")
	}
	return bot.messages[len(bot.messages)-1].text
}

func topic(chat, thread int64) telegram.Destination {
	return telegram.Destination{ChatID: chat, ThreadID: thread}
}

// The half a person sees. A status code on a phone screen is a fault report; what they need is the
// gesture that undoes it, and each of these is undone differently — one of them by waiting and
// doing nothing at all. Told the wrong one, they cancel a turn that was going to answer.
func TestATurnRefusedSaysWhatUndoesIt(t *testing.T) {
	for _, tc := range []struct {
		refusal string
		status  int
		want    string
	}{
		{"no_local_model", http.StatusServiceUnavailable, "modelo local"},
		{"turn_in_progress", http.StatusConflict, "/cancel"},
	} {
		t.Run(tc.refusal, func(t *testing.T) {
			bot := &recordingBot{}
			dc := &recordingDaemon{sendAssistantErr: &daemon.StatusError{
				Operation: "send assistant message",
				Status:    tc.status,
				Refusal:   tc.refusal,
			}}

			startTurn(bot, dc, NewTracker(), topic(-100123, 7), "procura")

			got := lastMessage(t, bot)
			if !strings.Contains(got, tc.want) {
				t.Errorf("message = %q, want the gesture %q that undoes this refusal", got, tc.want)
			}
			if strings.Contains(got, "status code") {
				t.Errorf("message = %q, want a sentence rather than a number", got)
			}
			// A refusal costs nothing and leaves nothing to cancel. Recording a turn id here would
			// point `/cancel` at the turn before this one, in a chat that is already confused about
			// why nothing happened.
			if len(bot.htmlMessages) != 0 {
				t.Errorf("html = %v, want the refusal sent as plain text", bot.htmlMessages)
			}
		})
	}
}

// A failure that is not a stated refusal — the daemon down, the socket cut — keeps the old wording.
// Inventing a gesture for it would send somebody to /cancel over a network cable.
func TestATurnThatFailedForNoStatedReasonStillSaysSo(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{sendAssistantErr: errors.New("perform request: connection refused")}

	startTurn(bot, dc, NewTracker(), topic(-100123, 7), "procura")

	got := lastMessage(t, bot)
	if !strings.Contains(got, "couldn't start turn") {
		t.Errorf("message = %q, want the unexplained-failure wording", got)
	}
	if strings.Contains(got, "/retomar") {
		t.Errorf("message = %q, want no invented remedy", got)
	}
}

// kindOf answers the empty string for a row whose kind cannot be read, and NOT formatFeed's
// "event".
//
// The distinction is not cosmetic. formatFeed's fallback is a LABEL for a line nobody could read;
// kindOf's answer is an INPUT to a decision. Reusing "event" would make an unreadable row match a
// family called "event" that somebody might one day write, and it would be silenced under a switch
// its owner never meant to cover it.
func TestKindOfIsEmptyNotEvent(t *testing.T) {
	cases := []struct {
		name string
		row  map[string]any
	}{
		{"a row with no kind at all", map[string]any{"id": float64(1)}},
		{"a kind that is not a string", map[string]any{"id": float64(1), "kind": float64(7)}},
		{"a null kind, as serde writes one", map[string]any{"id": float64(1), "kind": nil}},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := kindOf(c.row); got != "" {
				t.Fatalf("kindOf = %q, want the empty string", got)
			}
		})
	}

	if got := kindOf(map[string]any{"kind": "job_failed"}); got != "job_failed" {
		t.Fatalf("kindOf = %q, want the kind it was given", got)
	}
	// And the label path is untouched: formatFeed still says "event" for the same unreadable row.
	if label := formatFeed(map[string]any{"id": float64(1), "summary": "no kind here"}); !strings.HasPrefix(label, "event") {
		t.Fatalf("formatFeed = %q, want it to still label an unreadable row \"event\"", label)
	}
}

// Proposals, the kill switch and the budget are never silenced by the notification policy.
//
// Not because there is a list of exemptions to keep in step, but because they do not pass through
// the place the policy is consulted: the filter lives inside the GetFeed branch and nowhere else.
// That is the property this test pins — somebody who moves the filter up one level to "cover
// everything" makes the kill switch silenceable, and this is what tells them.
func TestGovernanceIsNeverSilenced(t *testing.T) {
	bot := &recordingBot{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	silenced := []map[string]any{{"id": float64(9), "kind": "job_failed", "summary": "a job failed"}}
	dc := &bootingDaemon{
		recordingDaemon: &recordingDaemon{
			notifyPolicy: notifier.Policy{
				Families: []notifier.Rule{{Selector: "job_", Enabled: false}},
			},
		},
		stopAfter:        8,
		stop:             cancel,
		arriving:         map[int][]map[string]any{3: {{"id": float64(1), "tool_name": "git push"}}},
		feedArriving:     map[int][]map[string]any{3: silenced, 4: silenced, 5: silenced, 6: silenced, 7: silenced},
		killFrom:         4,
		budgetPausedFrom: 5,
	}

	RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: 42}, nil, time.Millisecond)

	var texts []string
	for _, m := range bot.messages {
		texts = append(texts, m.text)
	}
	for _, m := range bot.htmlMessages {
		texts = append(texts, m.text)
	}
	joined := strings.Join(texts, "\n---\n")

	if strings.Contains(joined, "a job failed") {
		t.Errorf("the silenced feed line was sent anyway: %s", joined)
	}
	for _, wanted := range []string{"git push", "kill switch ENGAGED", "budget: PAUSED"} {
		if !strings.Contains(joined, wanted) {
			t.Errorf("governance went quiet: %q missing from\n%s", wanted, joined)
		}
	}
}

// Turning a family back on announces what happens next, never what was missed.
//
// The filter runs AFTER state.NewFeedItems, so a suppressed row still enters `seen`; re-enabling
// the family tomorrow cannot replay it. Move the filter before that call — which reads like the
// same thing and is cheaper — and flipping one switch dumps up to ninety days of backlog into the
// chat at once.
func TestReenablingAFamilyReplaysNothing(t *testing.T) {
	bot := &recordingBot{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	whileSilenced := map[string]any{"id": float64(1), "kind": "job_failed", "summary": "missed while off"}
	afterReenabling := map[string]any{"id": float64(2), "kind": "job_failed", "summary": "arrived while on"}
	both := []map[string]any{whileSilenced, afterReenabling}
	onlyFirst := []map[string]any{whileSilenced}

	dc := &bootingDaemon{
		recordingDaemon: &recordingDaemon{},
		stopAfter:       9,
		stop:            cancel,
		feedArriving: map[int][]map[string]any{
			2: onlyFirst, 3: onlyFirst, 4: onlyFirst,
			5: both, 6: both, 7: both, 8: both,
		},
		policyFrom: map[int]notifier.Policy{
			0: {Families: []notifier.Rule{{Selector: "job_", Enabled: false}}},
			5: {Families: []notifier.Rule{{Selector: "job_", Enabled: true}}},
		},
	}

	RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: 42}, nil, time.Millisecond)

	var joined string
	for _, m := range bot.messages {
		joined += m.text + "\n"
	}
	if strings.Contains(joined, "missed while off") {
		t.Errorf("re-enabling the family replayed the backlog:\n%s", joined)
	}
	// The counterpart, without which the assertion above would also pass on a notifier that sends
	// nothing at all.
	if !strings.Contains(joined, "arrived while on") {
		t.Errorf("re-enabling the family announced nothing new either:\n%s", joined)
	}
}

// A policy that cannot be read lets everything through.
//
// The failure the whole mechanism guards against is noise — and a guard against noise must never
// fail into silence, because silence is indistinguishable from everything being fine.
func TestPolicyReadFailsOpen(t *testing.T) {
	bot := &recordingBot{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	arriving := []map[string]any{{"id": float64(1), "kind": "job_failed", "summary": "still gets through"}}
	dc := &bootingDaemon{
		recordingDaemon: &recordingDaemon{
			// A policy that WOULD silence this line, served behind an error — so a read that
			// quietly succeeded would fail this test rather than pass it by accident.
			notifyPolicy: notifier.Policy{
				Families: []notifier.Rule{{Selector: "job_", Enabled: false}},
			},
			notifyPolicyErr: errors.New("the daemon went away mid-round"),
		},
		stopAfter:    6,
		stop:         cancel,
		feedArriving: map[int][]map[string]any{2: arriving, 3: arriving, 4: arriving, 5: arriving},
	}

	RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: 42}, nil, time.Millisecond)

	var joined string
	for _, m := range bot.messages {
		joined += m.text + "\n"
	}
	if !strings.Contains(joined, "still gets through") {
		t.Errorf("an unreadable policy silenced the feed:\n%s", joined)
	}
}

func TestNoteCommandCreatesANote(t *testing.T) {
	bot := &recordingBot{}
	dc := &recordingDaemon{}

	HandleMessage(bot, dc, NewTracker(), telegram.Destination{ChatID: 42}, "/note buy milk")

	if len(dc.noteCalls) != 1 || dc.noteCalls[0] != "buy milk" {
		t.Fatalf("CreateNote calls = %v, want [buy milk]", dc.noteCalls)
	}
	if dc.sendAssistantCalls != 0 {
		t.Errorf("SendAssistantMessage calls = %d, want 0", dc.sendAssistantCalls)
	}
	if len(bot.messages) != 1 || bot.messages[0].text != "noted (#17)" {
		t.Errorf("messages = %v, want one reply %q", bot.messages, "noted (#17)")
	}

	bare := &recordingBot{}
	HandleMessage(bare, dc, NewTracker(), telegram.Destination{ChatID: 42}, "/note")
	if len(dc.noteCalls) != 1 {
		t.Errorf("a bare /note called CreateNote: %v", dc.noteCalls)
	}
	if len(bare.messages) != 1 || bare.messages[0].text != "usage: /note <text>" {
		t.Errorf("messages = %v, want the usage line", bare.messages)
	}
}

func TestNoteFromAnUnauthorisedChatIsIgnored(t *testing.T) {
	cfg := config.Config{AllowedChatID: 42}
	bot := &recordingBot{}
	dc := &recordingDaemon{}

	HandleUpdate(bot, dc, fakeDownloader{}, cfg, NewTracker(), telegram.Update{
		Message: &telegram.Message{Chat: telegram.Chat{ID: 99}, From: &telegram.User{ID: 99}, Text: "/note buy milk"},
	})

	if len(dc.noteCalls) != 0 {
		t.Errorf("CreateNote calls = %v, want none", dc.noteCalls)
	}
}

func TestNoteTextNeverReachesTheLog(t *testing.T) {
	const secret = "the-secret-diary-entry"
	var buf bytes.Buffer
	log.SetOutput(&buf)
	defer log.SetOutput(os.Stderr)

	HandleMessage(&recordingBot{}, &recordingDaemon{}, NewTracker(), telegram.Destination{ChatID: 42}, "/note "+secret)
	HandleMessage(&recordingBot{}, &recordingDaemon{noteErr: errors.New("daemon unreachable")},
		NewTracker(), telegram.Destination{ChatID: 42}, "/note "+secret)

	if strings.Contains(buf.String(), secret) {
		t.Errorf("log carries the note text: %q", buf.String())
	}
}

// topicRouting runs the notifier over feed rows that arrive on the given polls.
func topicRouting(t *testing.T, bot *recordingBot, topics map[string]int64, arriving map[int][]map[string]any, policy notifier.Policy) {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	dc := &bootingDaemon{
		recordingDaemon: &recordingDaemon{notifyPolicy: policy},
		stopAfter:       7,
		stop:            cancel,
		feedArriving:    arriving,
	}
	RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: 42}, topics, time.Millisecond)
}

// sentFor returns the plain sends whose text contains needle.
func sentFor(bot *recordingBot, needle string) []sentMessage {
	var out []sentMessage
	for _, m := range bot.messages {
		if strings.Contains(m.text, needle) {
			out = append(out, m)
		}
	}
	return out
}

// A feed line whose project has a topic goes to that topic of the configured chat.
func TestAMappedProjectFeedLineGoesToItsTopic(t *testing.T) {
	bot := &recordingBot{}
	topicRouting(t, bot, map[string]int64{"proj-a": 77}, map[int][]map[string]any{
		3: {{"id": float64(31), "kind": "run", "project_id": "proj-a", "summary": "mapped line"}},
	}, notifier.Policy{})

	got := sentFor(bot, "mapped line")
	want := telegram.Destination{ChatID: 42, ThreadID: 77}
	if len(got) != 1 || got[0].to != want {
		t.Errorf("sends = %+v, want exactly one, to %+v", got, want)
	}
}

// No project, a null project and an unmapped project all stay on the configured chat.
func TestAnUnmappedOrMissingProjectGoesToTheConfiguredChat(t *testing.T) {
	bot := &recordingBot{}
	topicRouting(t, bot, map[string]int64{"proj-a": 77}, map[int][]map[string]any{
		3: {
			{"id": float64(31), "kind": "run", "summary": "no project line"},
			{"id": float64(32), "kind": "run", "project_id": nil, "summary": "null project line"},
			{"id": float64(33), "kind": "run", "project_id": "proj-z", "summary": "unmapped line"},
		},
	}, notifier.Policy{})

	configured := telegram.Destination{ChatID: 42}
	for _, needle := range []string{"no project line", "null project line", "unmapped line"} {
		got := sentFor(bot, needle)
		if len(got) != 1 || got[0].to != configured {
			t.Errorf("%q: sends = %+v, want exactly one, to %+v", needle, got, configured)
		}
	}
}

// A topic Telegram refuses must not loop forever: the line is re-sent once to the configured chat
// and is then done.
func TestARefusedTopicSendFallsBackToTheConfiguredChatOnce(t *testing.T) {
	topic := telegram.Destination{ChatID: 42, ThreadID: 77}
	bot := &recordingBot{sendErrFor: func(to telegram.Destination) error {
		if to == topic {
			return &telegram.APIError{Method: "sendMessage", StatusCode: 400, Description: "message thread not found"}
		}
		return nil
	}}
	row := []map[string]any{{"id": float64(31), "kind": "run", "project_id": "proj-a", "summary": "refused line"}}
	topicRouting(t, bot, map[string]int64{"proj-a": 77}, map[int][]map[string]any{3: row, 4: row, 5: row, 6: row}, notifier.Policy{})

	var toTopic, toChat int
	for _, m := range sentFor(bot, "refused line") {
		switch m.to {
		case topic:
			toTopic++
		case telegram.Destination{ChatID: 42}:
			toChat++
		}
	}
	if toTopic != 1 || toChat != 1 {
		t.Errorf("topic attempts = %d, configured-chat sends = %d, want 1 and 1: %+v", toTopic, toChat, bot.messages)
	}
}

// Throttling, a Telegram server error and an unreachable network say nothing about the topic, so the
// line is not redirected: it is forgotten and offered again next round, as before.
func TestAThrottledOrUnreachableTopicSendIsRetriedNotRedirected(t *testing.T) {
	failures := map[string]error{
		"throttled":                     &telegram.APIError{Method: "sendMessage", StatusCode: 429, RetryAfter: time.Second},
		"throttled without retry_after": &telegram.APIError{Method: "sendMessage", StatusCode: 429, Description: "Too Many Requests"},
		"server error":                  &telegram.APIError{Method: "sendMessage", StatusCode: 502, Description: "Bad Gateway"},
		"unreachable":                   &url.Error{Op: "Post", URL: "x", Err: errors.New("reset")},
	}
	for name, failure := range failures {
		t.Run(name, func(t *testing.T) {
			topic := telegram.Destination{ChatID: 42, ThreadID: 77}
			bot := &recordingBot{sendErrFor: func(to telegram.Destination) error {
				if to == topic {
					return failure
				}
				return nil
			}}
			row := []map[string]any{{"id": float64(31), "kind": "run", "project_id": "proj-a", "summary": "retried line"}}
			topicRouting(t, bot, map[string]int64{"proj-a": 77}, map[int][]map[string]any{3: row, 4: row, 5: row}, notifier.Policy{})

			var toTopic, toChat int
			for _, m := range sentFor(bot, "retried line") {
				switch m.to {
				case topic:
					toTopic++
				case telegram.Destination{ChatID: 42}:
					toChat++
				}
			}
			if toChat != 0 {
				t.Errorf("the line was redirected to the configured chat %d time(s): %+v", toChat, bot.messages)
			}
			if toTopic < 2 {
				t.Errorf("topic attempts = %d, want the line offered again on a later round (>= 2)", toTopic)
			}
		})
	}
}

// The policy still decides first: a suppressed line is not sent anywhere, topic included.
func TestAPolicySuppressedLineIsNotSentToItsTopic(t *testing.T) {
	bot := &recordingBot{}
	topicRouting(t, bot, map[string]int64{"proj-a": 77}, map[int][]map[string]any{
		3: {
			{"id": float64(31), "kind": "job_failed", "project_id": "proj-a", "summary": "silenced line"},
			{"id": float64(32), "kind": "run", "project_id": "proj-a", "summary": "allowed line"},
		},
	}, notifier.Policy{Families: []notifier.Rule{{Selector: "job_", Enabled: false}}})

	if got := sentFor(bot, "silenced line"); len(got) != 0 {
		t.Errorf("the suppressed line was sent: %+v", got)
	}
	// Without this the assertion above would also pass on a notifier that sends nothing.
	if got := sentFor(bot, "allowed line"); len(got) != 1 || got[0].to.ThreadID != 77 {
		t.Errorf("the allowed line = %+v, want one send to its topic", got)
	}
}

func TestACaptureRequestFeedLineIsSentWithoutItsKindPrefix(t *testing.T) {
	summary := "🧠 web · job #7 · job failed\nHá alguma coisa que só tu saibas sobre isto?\n(até às 14:30; depois o destilador avança) #cap7"
	got := formatFeed(map[string]any{"kind": "capture_requested", "summary": summary})
	if got != summary {
		t.Errorf("formatFeed = %q, want the summary alone", got)
	}
	if got := formatFeed(map[string]any{"kind": "job_failed", "summary": "x"}); got != "job_failed: x" {
		t.Errorf("other kinds lost their prefix: %q", got)
	}
}

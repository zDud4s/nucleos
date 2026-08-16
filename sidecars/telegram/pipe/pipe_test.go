package pipe

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"

	"nucleostelegram/config"
	"nucleostelegram/daemon"
	"nucleostelegram/telegram"
)

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
}

func (b *recordingBot) SendMessage(to telegram.Destination, text string) error {
	b.messages = append(b.messages, sentMessage{to: to, text: text})
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
	// lastChatID is the key the daemon was told to route on, which is the string an errand is
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
	t.Setenv("TMP", t.TempDir())
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
	t.Setenv("TMP", t.TempDir())
	if runtime.GOOS != "windows" {
		t.Skip("stands in for a transcriber with a Windows shell command")
	}

	bot := &recordingBot{}
	dc := &recordingDaemon{}
	// Stands in for a transcriber that heard "kill off": echo prints the transcript, rem swallows
	// the audio path this package appends.
	cfg := config.Config{AllowedChatID: 42, TranscribeCmd: "cmd /c echo /kill off&rem"}

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

	RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: 42}, time.Millisecond)

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

	RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: 42}, time.Millisecond)

	if len(bot.messages) != 1 || !strings.Contains(bot.messages[0].text, "git push") {
		t.Errorf("announcements = %v, want exactly the proposal that arrived after seeding", bot.messages)
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

	RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: 42}, time.Millisecond)

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

// errandDaemon answers the errand routes and records what it was asked to change, so a test can
// tell "said it did" apart from "did".
type errandDaemon struct {
	recordingDaemon
	errands   []daemon.Errand
	listErr   error
	createErr error
	created   [][2]string
	statuses  [][2]string
	brains    [][2]string
	closed    []int64
	nextID    int64
}

func (d *errandDaemon) ListErrands() ([]daemon.Errand, error) {
	return d.errands, d.listErr
}

func (d *errandDaemon) ErrandOfChat(chatKey string) (daemon.Errand, bool, error) {
	if d.listErr != nil {
		return daemon.Errand{}, false, d.listErr
	}
	for _, errand := range d.errands {
		if errand.ChatKey == chatKey {
			return errand, true, nil
		}
	}
	return daemon.Errand{}, false, nil
}

func (d *errandDaemon) CreateErrand(name, chatKey string) (int64, error) {
	d.created = append(d.created, [2]string{name, chatKey})
	if d.createErr != nil {
		return 0, d.createErr
	}
	d.nextID++
	return d.nextID, nil
}

func (d *errandDaemon) SetErrandStatus(id int64, status string) error {
	d.statuses = append(d.statuses, [2]string{strconv.FormatInt(id, 10), status})
	return nil
}

func (d *errandDaemon) SetErrandBrain(id int64, brain string) error {
	d.brains = append(d.brains, [2]string{strconv.FormatInt(id, 10), brain})
	return nil
}

func (d *errandDaemon) CloseErrand(id int64) error {
	d.closed = append(d.closed, id)
	return nil
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

func TestOpenAnErrandOnATopic(t *testing.T) {
	bot := &recordingBot{}
	dc := &errandDaemon{}

	HandleMessage(bot, dc, NewTracker(), topic(-100123, 7), "/assunto carros usados")

	if len(dc.created) != 1 || dc.created[0] != [2]string{"carros usados", "-100123:7"} {
		t.Fatalf("created = %v, want the name and the topic key", dc.created)
	}
	if !strings.Contains(lastMessage(t, bot), "carros usados") {
		t.Errorf("the confirmation should name the errand: %q", lastMessage(t, bot))
	}
}

// An errand IS a topic, so there is nowhere to put one in a chat that has no topics. Refusing here
// is what keeps a one-to-one chat from acquiring an errand that would then answer every message in
// it — including the ones that have nothing to do with the errand.
func TestOpeningAnErrandOutsideATopicIsRefused(t *testing.T) {
	bot := &recordingBot{}
	dc := &errandDaemon{}

	HandleMessage(bot, dc, NewTracker(), telegram.Destination{ChatID: -100123}, "/assunto carros")

	if len(dc.created) != 0 {
		t.Fatalf("created = %v, want nothing", dc.created)
	}
	if dc.sendAssistantCalls != 0 {
		t.Error("a refused command must not fall through and be answered as a question")
	}
	if !strings.Contains(strings.ToLower(lastMessage(t, bot)), "tópico") {
		t.Errorf("the refusal should say a topic is needed: %q", lastMessage(t, bot))
	}
}

func TestOpeningAnErrandWithNoNameAsksForOne(t *testing.T) {
	bot := &recordingBot{}
	dc := &errandDaemon{}

	HandleMessage(bot, dc, NewTracker(), topic(-100123, 7), "/assunto")

	if len(dc.created) != 0 {
		t.Fatalf("created = %v, want nothing", dc.created)
	}
	if dc.sendAssistantCalls != 0 {
		t.Error("a command missing its argument must not be sent to the model as a question")
	}
	if lastMessage(t, bot) == "" {
		t.Error("a command that did nothing has to say so")
	}
}

// One topic holds one errand — the daemon enforces it with a UNIQUE constraint and answers 409. The
// person who typed it gets a sentence, not a status code.
func TestOpeningASecondErrandOnOneTopicSaysWhy(t *testing.T) {
	bot := &recordingBot{}
	dc := &errandDaemon{createErr: errors.New("open errand: status code 409: ")}

	HandleMessage(bot, dc, NewTracker(), topic(-100123, 7), "/assunto outro")

	message := lastMessage(t, bot)
	if strings.Contains(message, "409") {
		t.Errorf("a status code is not an explanation: %q", message)
	}
	if message == "" {
		t.Error("the refusal has to be said out loud")
	}
}

func TestPauseResumeAndCloseActOnTheErrandOfThisTopic(t *testing.T) {
	here := daemon.Errand{ID: 4, Name: "carros", ChatKey: "-100123:7", Brain: "local", Status: "active"}
	elsewhere := daemon.Errand{ID: 5, Name: "casa", ChatKey: "-100123:9", Brain: "cloud", Status: "active"}

	for _, tc := range []struct {
		command string
		check   func(*testing.T, *errandDaemon)
	}{
		{"/pausa", func(t *testing.T, d *errandDaemon) {
			if len(d.statuses) != 1 || d.statuses[0] != [2]string{"4", "paused"} {
				t.Errorf("statuses = %v", d.statuses)
			}
		}},
		{"/retomar", func(t *testing.T, d *errandDaemon) {
			if len(d.statuses) != 1 || d.statuses[0] != [2]string{"4", "active"} {
				t.Errorf("statuses = %v", d.statuses)
			}
		}},
		{"/fim", func(t *testing.T, d *errandDaemon) {
			if len(d.closed) != 1 || d.closed[0] != 4 {
				t.Errorf("closed = %v, want the errand of this topic", d.closed)
			}
		}},
		{"/cerebro cloud", func(t *testing.T, d *errandDaemon) {
			if len(d.brains) != 1 || d.brains[0] != [2]string{"4", "cloud"} {
				t.Errorf("brains = %v", d.brains)
			}
		}},
	} {
		t.Run(tc.command, func(t *testing.T) {
			bot := &recordingBot{}
			dc := &errandDaemon{errands: []daemon.Errand{elsewhere, here}}

			HandleMessage(bot, dc, NewTracker(), topic(-100123, 7), tc.command)

			tc.check(t, dc)
			if lastMessage(t, bot) == "" {
				t.Error("a command that changed something has to say so")
			}
		})
	}
}

// The same commands in a topic with no errand change nothing and say so. Silence here reads as
// success, and the next message would go to a model nobody moved.
func TestErrandCommandsInATopicWithNoErrandChangeNothing(t *testing.T) {
	for _, command := range []string{"/pausa", "/retomar", "/fim", "/cerebro cloud"} {
		t.Run(command, func(t *testing.T) {
			bot := &recordingBot{}
			dc := &errandDaemon{}

			HandleMessage(bot, dc, NewTracker(), topic(-100123, 7), command)

			if len(dc.statuses)+len(dc.brains)+len(dc.closed) != 0 {
				t.Errorf("something was changed: %v %v %v", dc.statuses, dc.brains, dc.closed)
			}
			if dc.sendAssistantCalls != 0 {
				t.Error("an errand command must not fall through to the model")
			}
			if !strings.Contains(strings.ToLower(lastMessage(t, bot)), "assunto") {
				t.Errorf("the answer should say there is no errand here: %q", lastMessage(t, bot))
			}
		})
	}
}

// `/cerebro` with a word the daemon does not know changes nothing. `Brain::from_wire` resolves
// anything unrecognised to a default, so passing a typo through would move the errand silently.
func TestAnUnknownBrainChangesNothing(t *testing.T) {
	bot := &recordingBot{}
	dc := &errandDaemon{errands: []daemon.Errand{
		{ID: 4, Name: "carros", ChatKey: "-100123:7", Brain: "local", Status: "active"},
	}}

	HandleMessage(bot, dc, NewTracker(), topic(-100123, 7), "/cerebro nuvem")

	if len(dc.brains) != 0 {
		t.Errorf("brains = %v, want nothing changed", dc.brains)
	}
	if dc.sendAssistantCalls != 0 {
		t.Error("an unrecognised brain must not be sent to the model as a question")
	}
	if !strings.Contains(lastMessage(t, bot), "local") {
		t.Errorf("the refusal should name the two that exist: %q", lastMessage(t, bot))
	}
}

func TestListErrandsNamesTheTopicOfEachOne(t *testing.T) {
	bot := &recordingBot{}
	dc := &errandDaemon{errands: []daemon.Errand{
		{ID: 4, Name: "carros", ChatKey: "-100123:7", Brain: "local", Status: "active"},
		{ID: 5, Name: "casa", ChatKey: "-100123:9", Brain: "cloud", Status: "paused"},
	}}

	HandleMessage(bot, dc, NewTracker(), topic(-100123, 7), "/assuntos")

	said := lastMessage(t, bot)
	for _, want := range []string{"carros", "casa", "paused", "cloud"} {
		if !strings.Contains(said, want) {
			t.Errorf("the listing should mention %q: %q", want, said)
		}
	}
}

func TestListErrandsWithNoneSaysSo(t *testing.T) {
	bot := &recordingBot{}
	dc := &errandDaemon{}

	HandleMessage(bot, dc, NewTracker(), topic(-100123, 7), "/assuntos")

	if dc.sendAssistantCalls != 0 {
		t.Error("/assuntos must not fall through to the model")
	}
	if !strings.Contains(lastMessage(t, bot), "/assunto") {
		t.Errorf("an empty list should say how to open one: %q", lastMessage(t, bot))
	}
}

// The errand routes on the plain recording daemon: present so it still satisfies Daemon, and inert
// so a test that did not ask about errands cannot quietly exercise them.
func (d *recordingDaemon) ListErrands() ([]daemon.Errand, error) { return nil, nil }
func (d *recordingDaemon) ErrandOfChat(string) (daemon.Errand, bool, error) {
	return daemon.Errand{}, false, nil
}
func (d *recordingDaemon) CreateErrand(string, string) (int64, error) { return 0, nil }
func (d *recordingDaemon) SetErrandStatus(int64, string) error        { return nil }
func (d *recordingDaemon) SetErrandBrain(int64, string) error         { return nil }
func (d *recordingDaemon) CloseErrand(int64) error                    { return nil }

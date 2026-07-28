package pipe

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"nucleostelegram/config"
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
	// buttonErrOnce fails the first button send only, standing in for a send that is lost while
	// the connection is down and works again on the next attempt.
	buttonErrOnce error
	buttonCalls   int
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
	b.buttonCalls++
	b.messages = append(b.messages, sentMessage{chatID: chatID, text: text})
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
	run                map[string]any
	runErr             error
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

func (d *recordingDaemon) SendAssistantMessage(string, string) (int64, error) {
	d.sendAssistantCalls++
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
	resolveIncoming(dl, "", &telegram.Message{Voice: &telegram.Voice{FileID: "v1"}})

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

	got := resolveIncoming(dl, "", msg)
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

	sendReply(bot, 42, "**bold**")

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

	RunNotifier(ctx, bot, dc, 42, time.Millisecond)

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

	RunNotifier(ctx, bot, dc, 42, time.Millisecond)

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

	RunNotifier(ctx, bot, dc, 42, time.Millisecond)

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

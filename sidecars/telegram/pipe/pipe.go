package pipe

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"nucleostelegram/config"
	"nucleostelegram/daemon"
	"nucleostelegram/format"
	"nucleostelegram/notifier"
	"nucleostelegram/shortcuts"
	"nucleostelegram/telegram"
	"nucleostelegram/transcribe"
)

const pollInterval = 3 * time.Second
const maxPollAttempts = 200

// A turn outlives a daemon restart — the run keeps going server-side — so a failed GetRun is not a
// reason to stop watching. A daemon that is gone for good still has to produce an answer, which is
// what bounds the tolerance.
const maxConsecutivePollErrors = 5

const tempFilePattern = "nucleos-tg-*"

// An attachment has to survive the turn that reads it, and a queued turn can take minutes — but
// these are files from a private control channel sitting unencrypted in the system temp directory,
// and "until someone notices" is not a retention policy.
const tempFileTTL = 24 * time.Hour

const helpText = `Talk normally to reach the orchestrator.

/proposals — list pending proposals
/budget — show the current budget state
/kill on|off — engage or disengage the kill switch
/cancel — cancel the last turn in this chat
/projects — list registered projects (no agent)
/proj [name] — list registered projects, optionally filtered by name
/inbox — show what is waiting in the mailbox (free)
/mail — read and classify what is waiting (costs a run)
/help — show this help`

type Bot interface {
	SendMessage(to telegram.Destination, text string) error
	SendHTML(to telegram.Destination, html string) error
	SendMessageWithButtons(to telegram.Destination, text string, rows [][]telegram.Button) error
	AnswerCallbackQuery(callbackID, text string) error
}

type Daemon interface {
	SendAssistantMessage(chatKey, text string) (int64, error)
	GetRun(id int64) (map[string]any, error)
	GetProposals() ([]map[string]any, error)
	// GetRefusedActions is what the injection barrier turned away. A separate route from the one
	// above, because these carry no approve and no reject — nothing is held, so there is nothing to
	// let through or release.
	GetRefusedActions() ([]map[string]any, error)
	GetProjects() ([]map[string]any, error)
	ApproveProposal(id int64) (map[string]any, error)
	RejectProposal(id int64) error
	GetFeed() ([]map[string]any, error)
	// GetNotifyPolicy is which feed kinds the owner still wants forwarded. Beside GetFeed because
	// it is read in the same breath, and read only there: the governance lines below — the kill
	// switch and the budget — never consult it, which is how they stay unsilenceable without an
	// exception list to remember.
	GetNotifyPolicy() (notifier.Policy, error)
	GetBudget() (map[string]any, error)
	GetKill() (bool, error)
	TriageEmail() (map[string]any, error)
	GetEmailQueue() ([]map[string]any, error)
	SetKill(engaged bool) error
	CancelRun(id int64) error
}

type Downloader interface {
	GetFile(fileID string) (string, error)
	DownloadFile(filePath string) ([]byte, error)
}

// Tracker remembers the last turn of each conversation, so `/cancel` knows what to cancel.
//
// Keyed on the chat KEY and not on the chat id: two topics of one group are two conversations, the
// núcleo hands each its own turn, and a `/cancel` typed in one of them must not reach into the
// other. It is the same string the daemon routes on, so the two cannot disagree about what "this
// conversation" means.
type Tracker struct {
	mu       sync.Mutex
	lastTurn map[string]int64
}

func NewTracker() *Tracker {
	return &Tracker{lastTurn: map[string]int64{}}
}

func (t *Tracker) Set(key string, turnID int64) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.lastTurn[key] = turnID
}

func (t *Tracker) Get(key string) (int64, bool) {
	t.mu.Lock()
	defer t.mu.Unlock()
	v, ok := t.lastTurn[key]
	return v, ok
}

// ChatKey is how a conversation is named everywhere outside Telegram: in the daemon's `chat_id`, in
// the tracker, and as the queue an update is serialised on.
//
// A topic gets `<chat>:<thread>`. Everything else gets the bare chat id — byte for byte the key
// this sidecar has always sent. That is not a tidiness point: a one-to-one chat that started
// answering to `-100…:0` on deploy day would have lost its session and its history, and nothing
// would have failed loudly enough to notice.
func ChatKey(to telegram.Destination) string {
	key := strconv.FormatInt(to.ChatID, 10)
	if to.ThreadID != 0 {
		key += ":" + strconv.FormatInt(to.ThreadID, 10)
	}
	return key
}

// DestinationFromKey is ChatKey read backwards: the topic a key names, or a refusal.
//
// It exists because keys now travel in the other direction. Every key this sidecar held used to come
// from an update it had just received, so a `telegram.Destination` was always at hand; a key that
// arrives from the daemon has no update behind it, and this is how it is turned back into a topic.
//
// Split on the LAST colon, though neither half can contain one — a chat id and a thread id are both
// integers. Taking the last is what makes a malformed key with two colons fail on the parse instead
// of silently reading `-100:7` as a chat id, which is the shape a wrong answer would take.
//
// The refusal is the point of the second return value. The fallback nobody writes on purpose is
// "send it to the usual place", and the usual place is the configured chat — a group's General,
// where a topic's own messages do not belong. A missing notification is a thing somebody
// notices; a notification in the wrong room is not.
func DestinationFromKey(key string) (telegram.Destination, bool) {
	chat, thread := key, ""
	if cut := strings.LastIndex(key, ":"); cut >= 0 {
		chat, thread = key[:cut], key[cut+1:]
	}

	chatID, err := strconv.ParseInt(chat, 10, 64)
	if err != nil {
		return telegram.Destination{}, false
	}
	if thread == "" {
		// No colon at all: a one-to-one chat or a group's General, exactly as `ChatKey` writes it.
		// A key ENDING in a colon is a different thing and lands in the parse below, where it fails.
		if !strings.Contains(key, ":") {
			return telegram.Destination{ChatID: chatID}, true
		}
		return telegram.Destination{}, false
	}

	threadID, err := strconv.ParseInt(thread, 10, 64)
	if err != nil {
		return telegram.Destination{}, false
	}
	return telegram.Destination{ChatID: chatID, ThreadID: threadID}, true
}

// DestinationOf is where an update came from, and therefore where its answer goes.
func DestinationOf(u telegram.Update) telegram.Destination {
	switch {
	case u.CallbackQuery != nil && u.CallbackQuery.Message != nil:
		return u.CallbackQuery.Message.Destination()
	case u.Message != nil:
		return u.Message.Destination()
	default:
		return telegram.Destination{}
	}
}

func HandleUpdate(bot Bot, dc Daemon, dl Downloader, cfg config.Config, tr *Tracker, u telegram.Update) {
	if u.CallbackQuery != nil {
		cb := u.CallbackQuery
		if cb.Message == nil || !cfg.IsAllowed(cb.Message.Chat.ID) {
			return
		}
		// The button is checked against the finger that pressed it, not only against the chat it
		// lives in: in a group those are different questions, and this button approves an action an
		// autonomous agent asked to take.
		if !cfg.IsAllowedSender(cb.Message.Chat.ID, cb.From.ID) {
			logSend("unauthorised press", bot.AnswerCallbackQuery(cb.ID, "not authorised"))
			log.Printf("callback from unauthorised user %d in chat %d ignored", cb.From.ID, cb.Message.Chat.ID)
			return
		}
		HandleCallback(bot, dc, *cb)
		return
	}
	if u.Message == nil {
		return
	}
	if !cfg.IsAllowed(u.Message.Chat.ID) {
		return
	}
	if !cfg.IsAllowedSender(u.Message.Chat.ID, u.Message.SenderID()) {
		log.Printf("message from unauthorised user %d in chat %d ignored", u.Message.SenderID(), u.Message.Chat.ID)
		return
	}

	to := u.Message.Destination()
	text := resolveIncoming(dl, dc, cfg.TranscribeCmd, u.Message)
	if strings.TrimSpace(text) == "" {
		// A sticker, a location, a poll: none of them carries a prompt, and a turn started on an
		// empty one spends a run to answer nothing.
		logSend("unreadable message", bot.SendMessage(to,
			"I can only read text, voice notes, photos and documents."))
		return
	}
	if u.Message.Voice != nil {
		handleTranscript(bot, dc, tr, to, text)
		return
	}
	HandleMessage(bot, dc, tr, to, text)
}

// buildAttachmentPrompt returns the orchestrator prompt for a saved attachment. A document takes
// priority over a photo. Returns ("", false) when neither path is set. The caption is carried
// through because it is where the instruction lives — without it the orchestrator is handed a file
// and no idea what it is meant to do with it.
func buildAttachmentPrompt(documentPath, photoPath, caption string) (string, bool) {
	var prompt string
	switch {
	case documentPath != "":
		prompt = "The user sent a document. File saved at: " + documentPath
	case photoPath != "":
		prompt = "The user sent a photo. File saved at: " + photoPath
	default:
		return "", false
	}

	if caption = strings.TrimSpace(caption); caption != "" {
		prompt += "\nThe user wrote: " + caption
	}
	return prompt, true
}

func downloadToTemp(dl Downloader, fileID, suffix string) (string, error) {
	// Swept here rather than on a timer: this is the only place that creates these files, so it is
	// the one place guaranteed to run whenever there are new ones to eventually clean up.
	sweepTempFiles(os.TempDir(), tempFileTTL)

	remotePath, err := dl.GetFile(fileID)
	if err != nil {
		return "", err
	}
	data, err := dl.DownloadFile(remotePath)
	if err != nil {
		return "", err
	}
	f, err := os.CreateTemp("", tempFilePattern+suffix)
	if err != nil {
		return "", err
	}
	defer f.Close()
	if _, err := f.Write(data); err != nil {
		return "", err
	}
	return f.Name(), nil
}

// sweepTempFiles removes attachments this sidecar wrote and nobody came back for. It only ever
// touches names it creates itself, so a shared temp directory keeps everything else.
func sweepTempFiles(dir string, maxAge time.Duration) {
	matches, err := filepath.Glob(filepath.Join(dir, tempFilePattern))
	if err != nil {
		return
	}
	cutoff := time.Now().Add(-maxAge)
	for _, path := range matches {
		info, err := os.Stat(path)
		if err != nil || info.IsDir() || info.ModTime().After(cutoff) {
			continue
		}
		if err := os.Remove(path); err != nil {
			log.Printf("could not remove stale attachment %q: %v", path, err)
		}
	}
}

// resolveIncoming turns a message (possibly a voice note or attachment) into the text to route to
// the orchestrator. Voice → transcript (or a graceful "unavailable" note); document/photo → a
// "file saved at" prompt; otherwise the plain text. Never returns an error — it degrades to a
// human-readable note the orchestrator can relay.
// voiceCapturer is the daemon's voice pillar, asked for by type assertion rather than added to
// `Daemon`.
//
// Optional on purpose, and in two directions: a daemon build that predates the pillar does not have
// this method, and a daemon that has it may still have voice switched off. Widening `Daemon` would
// have forced every stub in this package to grow a method most of them do not care about, and would
// have implied the capability is required when it is not.
type voiceCapturer interface {
	VoiceCapture(audio []byte, format string, durationMs int64) (string, bool, error)
}

func resolveIncoming(dl Downloader, dc Daemon, transcribeCmd string, msg *telegram.Message) string {
	switch {
	case msg.Voice != nil:
		path, err := downloadToTemp(dl, msg.Voice.FileID, ".ogg")
		if err != nil {
			return "[voice message received but could not be downloaded]"
		}
		// The recording is a person's voice, in the clear, in the system temp directory; it is
		// needed for exactly as long as the transcription takes and not one turn longer.
		defer func() {
			if err := os.Remove(path); err != nil {
				log.Printf("could not remove voice recording %q: %v", path, err)
			}
		}()
		// The núcleo first, when it can. It spawns the transcriber, applies the operator's
		// misheard-word hints and runs the local cleanup model — none of which this process can do,
		// and all of which a voice note deserves as much as a dictation does. Asking it here is what
		// stops this file being a second implementation of the same contract.
		//
		// Only "I cannot" sends us to the local command: voice switched off, or a daemon that is not
		// answering. A voice note is somebody talking to you, and it must not stop arriving because an
		// optional pillar is unconfigured.
		if vc, ok := dc.(voiceCapturer); ok {
			if audio, readErr := os.ReadFile(path); readErr == nil {
				text, configured, err := vc.VoiceCapture(audio, "ogg", int64(msg.Voice.Duration)*1000)
				if configured {
					if err != nil {
						log.Printf("voice capture via the daemon failed: %v", err)
						return "[voice message received but transcription is unavailable]"
					}
					if strings.TrimSpace(text) == "" {
						return "[voice message received but nothing was heard]"
					}
					return text
				}
			} else {
				log.Printf("could not read the downloaded voice recording: %v", readErr)
			}
		}

		text, err := transcribe.Transcribe(transcribeCmd, path)
		if err != nil || strings.TrimSpace(text) == "" {
			return "[voice message received but transcription is unavailable]"
		}
		return text
	case msg.Document != nil:
		path, err := downloadToTemp(dl, msg.Document.FileID, docSuffix(msg.Document.FileName))
		if err != nil {
			return "[document received but could not be downloaded]"
		}
		prompt, _ := buildAttachmentPrompt(path, "", msg.Caption)
		return prompt
	case len(msg.Photo) > 0:
		// largest photo is the last element in Telegram's ascending-size array
		best := msg.Photo[len(msg.Photo)-1]
		path, err := downloadToTemp(dl, best.FileID, ".jpg")
		if err != nil {
			return "[photo received but could not be downloaded]"
		}
		prompt, _ := buildAttachmentPrompt("", path, msg.Caption)
		return prompt
	default:
		return msg.Text
	}
}

// logSend records a send that failed instead of dropping it. This channel is a remote control: an
// approval prompt or a kill-switch alert that never arrives leaves a person looking at a bot that
// seems alive, unaware a decision was taken without them. The failure has to land somewhere.
func logSend(what string, err error) {
	if err == nil {
		return
	}
	log.Printf("telegram send failed (%s): %v", what, err)
}

func docSuffix(name string) string {
	if i := strings.LastIndex(name, "."); i >= 0 {
		return name[i:]
	}
	return ""
}

func HandleMessage(bot Bot, dc Daemon, tr *Tracker, to telegram.Destination, text string) {
	handleIntent(bot, dc, tr, to, shortcuts.Route(text))
}

// handleTranscript routes what a speech model heard rather than what someone typed. The difference
// is that a transcript cannot fire a command — see shortcuts.RouteTranscript.
func handleTranscript(bot Bot, dc Daemon, tr *Tracker, to telegram.Destination, text string) {
	handleIntent(bot, dc, tr, to, shortcuts.RouteTranscript(text))
}

func handleIntent(bot Bot, dc Daemon, tr *Tracker, to telegram.Destination, intent shortcuts.Intent) {
	switch intent.Kind {
	case shortcuts.Help:
		logSend("help", bot.SendMessage(to, helpText))
	case shortcuts.Refused:
		// Said out loud, never silently: a person who spoke `/kill off` and heard nothing back
		// would reasonably believe the kill switch is now off.
		logSend("refused spoken command", bot.SendMessage(to,
			"I heard a command in that voice note and did not run it — type it instead: "+
				strings.TrimSpace(intent.Text)))
	case shortcuts.Kill:
		if err := dc.SetKill(intent.On); err != nil {
			logSend("kill switch error", bot.SendMessage(to, "kill switch error: "+err.Error()))
			return
		}
		if intent.On {
			logSend("kill switch engaged", bot.SendMessage(to, "kill switch ENGAGED"))
		} else {
			logSend("kill switch disengaged", bot.SendMessage(to, "kill switch disengaged"))
		}
	case shortcuts.Cancel:
		id, ok := tr.Get(ChatKey(to))
		if !ok {
			logSend("nothing to cancel", bot.SendMessage(to, "nothing to cancel"))
			return
		}
		if err := dc.CancelRun(id); err != nil {
			logSend("cancel error", bot.SendMessage(to, err.Error()))
			return
		}
		logSend("cancel confirmation", bot.SendMessage(to, fmt.Sprintf("cancelled turn %d", id)))
	case shortcuts.Budget:
		budget, err := dc.GetBudget()
		if err != nil {
			logSend("budget error", bot.SendMessage(to, err.Error()))
			return
		}
		logSend("budget", bot.SendMessage(to, formatBudget(budget)))
	case shortcuts.Proposals:
		sendProposals(bot, dc, to)
	case shortcuts.Proj:
		sendProjects(bot, dc, to, intent.Arg)
	case shortcuts.Mail:
		sendTriage(bot, dc, to)
	case shortcuts.Inbox:
		sendInbox(bot, dc, to)
	default:
		startTurn(bot, dc, tr, to, intent.Text)
	}
}

// refusalMessage says what was refused and what undoes it.
//
// The núcleo names the refusal and this says the sentence, which is the only split that works:
// `/cancel` is a Telegram command and the núcleo must not know it exists, while the reason a turn
// was refused is a fact only the núcleo has. Each is undone differently, and one of them is undone
// by doing nothing at all — told the wrong one, a person cancels a turn that was about to answer.
//
// A refusal the sidecar does not recognise keeps the old wording. That covers a núcleo newer than
// this binary as well as a cut cable, and both are better served by "something failed, here it is"
// than by a remedy invented to fill the gap.
func refusalMessage(err error) string {
	var refused *daemon.StatusError
	if errors.As(err, &refused) {
		switch refused.Refusal {
		case "no_local_model":
			return "Esta conversa está no modelo local e não há nenhum configurado."
		case "turn_in_progress":
			return "Ainda estou a responder à mensagem anterior. Ou esperas, ou /cancel."
		}
	}
	return "couldn't start turn: " + err.Error()
}

func startTurn(bot Bot, dc Daemon, tr *Tracker, to telegram.Destination, text string) {
	turnID, err := dc.SendAssistantMessage(ChatKey(to), text)
	if err != nil {
		logSend("turn start failure", bot.SendMessage(to, refusalMessage(err)))
		return
	}
	tr.Set(ChatKey(to), turnID)
	logSend("turn acknowledgement", bot.SendMessage(to, fmt.Sprintf("working… (turn %d)", turnID)))
	GoGuarded("turn watcher", func() {
		reply := pollTurnAndReply(dc, turnID, pollInterval, maxPollAttempts)
		sendReply(bot, to, reply)
	})
}

// sendReply renders the orchestrator's Markdown reply to Telegram HTML, splits it under the 4096
// cap, and sends each chunk with parse_mode=HTML; if a chunk is rejected (invalid entities), it
// falls back to plain text so the user always receives the content.
func sendReply(bot Bot, to telegram.Destination, reply string) {
	for _, chunk := range format.Chunk(format.ToHTML(reply), 4096) {
		err := bot.SendHTML(to, chunk)
		if err == nil {
			continue
		}
		// The fallback exists for one failure: Telegram rejecting the markup. When the send failed
		// because we are throttled or because it never arrived, the content was never the problem —
		// re-sending it immediately doubles this sidecar's traffic at the exact moment Telegram
		// asked it to send less.
		if telegram.IsRateLimited(err) || telegram.IsTransport(err) {
			logSend("reply chunk", err)
			continue
		}
		logSend("reply chunk (retrying as plain text)", err)
		logSend("reply chunk fallback", bot.SendMessage(to, format.StripTags(chunk)))
	}
}

func pollTurnAndReply(dc Daemon, turnID int64, interval time.Duration, maxAttempts int) string {
	consecutiveErrors := 0
	for i := 0; i < maxAttempts; i++ {
		run, err := dc.GetRun(turnID)
		if err != nil {
			// The run is the daemon's, not ours: it keeps going while the daemon restarts, and
			// giving up on the first error threw away output that was still coming.
			consecutiveErrors++
			if consecutiveErrors >= maxConsecutivePollErrors {
				return "error checking turn: " + err.Error()
			}
			time.Sleep(interval)
			continue
		}
		consecutiveErrors = 0
		status, _ := run["status"].(string)
		if isTerminal(status) {
			return runReply(run)
		}
		time.Sleep(interval)
	}
	return "turn still running — check /budget or the dashboard"
}

func isTerminal(status string) bool {
	switch status {
	case "completed", "failed", "timed_out", "cancelled", "interrupted":
		return true
	default:
		return false
	}
}

func runReply(run map[string]any) string {
	status := run["status"].(string)
	switch status {
	case "completed":
		stdout, _ := run["stdout"].(string)
		if strings.TrimSpace(stdout) == "" {
			return "(done, no output)"
		}
		return stdout
	case "failed":
		return "turn failed: " + stderrOr(run)
	case "timed_out":
		return "turn timed out"
	case "cancelled":
		return "turn cancelled"
	case "interrupted":
		return "turn interrupted"
	default:
		return "turn ended: " + status
	}
}

func stderrOr(run map[string]any) string {
	stderr, _ := run["stderr"].(string)
	if strings.TrimSpace(stderr) == "" {
		return "unknown error"
	}
	return stderr
}

func formatBudget(b map[string]any) string {
	paused, _ := b["paused"].(bool)
	if !paused {
		return "budget: active"
	}

	result := "budget: PAUSED"
	if reason, ok := b["reason"].(string); ok && reason != "" {
		result += " (" + reason + ")"
	}
	return result
}

func formatProposal(p map[string]any) string {
	result := fmt.Sprintf("proposal %d: %s", idOf(p), strOr(p, "tool_name", "?"))
	if reasoning, ok := p["reasoning"].(string); ok && reasoning != "" {
		result += "\nwhy: " + reasoning
	}
	// Type assertion and not key presence. The field is always THERE — serde writes
	// `"project_id": null` for anything that has none — so testing presence printed
	// "project: <nil>" under every machine-wide proposal. It looks like a
	// cosmetic slip and it is not: this is the line somebody reads to decide what they are
	// approving, and a rendered null is noise in exactly the place that has to be legible.
	if project, ok := p["project_id"].(string); ok && project != "" {
		result += "\nproject: " + project
	}
	return result
}

func idOf(m map[string]any) int64 {
	switch value := m["id"].(type) {
	case float64:
		return int64(value)
	case json.Number:
		id, err := value.Int64()
		if err == nil {
			return id
		}
	}
	return 0
}

func strOr(m map[string]any, key, fallback string) string {
	value, ok := m[key].(string)
	if !ok {
		return fallback
	}
	return value
}

// kindOf is the feed row's kind as the policy sees it, and deliberately NOT formatFeed's
// `strOr(f, "kind", "event")`. That fallback is a LABEL for a line nobody could read; this one is
// an INPUT to a decision, and "event" would make an unreadable line match a family nobody wrote —
// silencing it under a switch whose owner never meant it to cover this. The empty string is the
// honest answer, and Allows passes it.
//
// strOr does `m[key].(string)`, so a kind that is not a string falls back exactly as a missing one
// does. Both cases are tested.
func kindOf(f map[string]any) string { return strOr(f, "kind", "") }

func parseCallback(data string) (action string, id int64, ok bool) {
	action, rawID, found := strings.Cut(data, ":")
	if !found || (action != "approve" && action != "reject") {
		return "", 0, false
	}
	id, err := strconv.ParseInt(rawID, 10, 64)
	if err != nil {
		return "", 0, false
	}
	return action, id, true
}

func approveRejectRow(id int64) [][]telegram.Button {
	formattedID := strconv.FormatInt(id, 10)
	return [][]telegram.Button{{
		{Text: "✅ Approve", CallbackData: "approve:" + formattedID},
		{Text: "❌ Reject", CallbackData: "reject:" + formattedID},
	}}
}

func sendProposals(bot Bot, dc Daemon, to telegram.Destination) {
	props, err := dc.GetProposals()
	if err != nil {
		logSend("proposals error", bot.SendMessage(to, "couldn't fetch proposals: "+err.Error()))
		return
	}
	// Read before the early return below, so a machine with nothing to approve and something
	// refused does not answer "no pending proposals" and hide the second list entirely.
	//
	// A failure here is reported and does not take the approvable ones down with it: the two lists
	// are separate routes and the one that answered is still worth showing.
	refused, refusedErr := dc.GetRefusedActions()
	if refusedErr != nil {
		logSend("refused actions error", bot.SendMessage(to,
			"couldn't fetch what the barrier refused: "+refusedErr.Error()))
	}

	if len(props) == 0 && len(refused) == 0 {
		logSend("no proposals", bot.SendMessage(to, "no pending proposals"))
		return
	}
	for _, p := range props {
		logSend("proposal", bot.SendMessageWithButtons(to, formatProposal(p), approveRejectRow(idOf(p))))
	}
	// No buttons. Neither /approve nor /reject works on one of these — both answer 409 for anything
	// that is not an action-approval — and there is nothing held to release: the turn was denied and
	// carried on. What this is for is that somebody finds out, does the thing themselves, or asks
	// the agent again in a turn that starts clean and may act.
	for _, r := range refused {
		logSend("refused action", bot.SendMessage(to, "recusado pela barreira — "+formatProposal(r)))
	}
}

// sendTriage spends a run, deliberately and only here: `/mail` is the whole point of the pillar
// being on demand. It answers with what it STARTED, not with verdicts — a run takes minutes, and
// the verdicts arrive by themselves through the feed notifier.
func sendTriage(bot Bot, dc Daemon, to telegram.Destination) {
	outcome, err := dc.TriageEmail()
	if err != nil {
		logSend("triage error", bot.SendMessage(to, "couldn't triage the mailbox: "+err.Error()))
		return
	}
	queued := 0
	if n, ok := outcome["queued"].(float64); ok {
		queued = int(n)
	}
	if _, started := outcome["run_id"].(float64); !started {
		reason := "nothing to do"
		if r, ok := outcome["reason"].(string); ok && r != "" {
			reason = r
		}
		logSend("triage not started", bot.SendMessage(to, "no triage started: "+reason))
		return
	}
	logSend("triage started", bot.SendMessage(to, fmt.Sprintf(
		"reading %d message(s) — the verdicts arrive here in a few minutes", queued)))
}

// sendInbox costs nothing: it reports what is already known, which is what makes it safe to ask
// for at any time.
func sendInbox(bot Bot, dc Daemon, to telegram.Destination) {
	queue, err := dc.GetEmailQueue()
	if err != nil {
		logSend("mailbox error", bot.SendMessage(to, "couldn't read the mailbox: "+err.Error()))
		return
	}
	if len(queue) == 0 {
		logSend("empty mailbox", bot.SendMessage(to, "nothing in the mailbox yet"))
		return
	}

	var waiting, judged []string
	for _, mail := range queue {
		who := strOr(mail, "from_name", "")
		if who == "" {
			who = strOr(mail, "from_addr", "(unknown sender)")
		}
		class, triaged := mail["triage_class"].(string)
		if !triaged || class == "" {
			waiting = append(waiting, fmt.Sprintf("· %s — %s",
				who, strOr(mail, "subject", "(no subject)")))
			continue
		}
		if len(judged) < 10 {
			judged = append(judged, fmt.Sprintf("[%s] %s — %s",
				class, who, strOr(mail, "triage_summary", strOr(mail, "subject", ""))))
		}
	}

	var out []string
	if len(waiting) > 0 {
		out = append(out, fmt.Sprintf("waiting to be read (%d) — send /mail to triage:", len(waiting)))
		out = append(out, waiting...)
	}
	if len(judged) > 0 {
		out = append(out, "", "already read:")
		out = append(out, judged...)
	}
	logSend("inbox", bot.SendMessage(to, strings.Join(out, "\n")))
}

func sendProjects(bot Bot, dc Daemon, to telegram.Destination, filter string) {
	projects, err := dc.GetProjects()
	if err != nil {
		logSend("projects error", bot.SendMessage(to, "couldn't fetch projects: "+err.Error()))
		return
	}
	var records []string
	for _, p := range projects {
		id := strOr(p, "project_id", "(sem id)")
		if filter != "" && !strings.EqualFold(id, filter) && !strings.Contains(strings.ToLower(id), strings.ToLower(filter)) {
			continue
		}
		mode := strings.ToLower(strOr(p, "mode", "?"))
		pending := 0
		if n, ok := p["pending"].(float64); ok {
			pending = int(n)
		}
		root := "—"
		if r, ok := p["project_root"].(string); ok && r != "" {
			root = r
		}
		records = append(records, fmt.Sprintf("**%s**\nModo: %s · Pendentes: %d\nRaiz: %s", id, mode, pending, root))
	}
	if len(records) == 0 {
		if filter != "" {
			logSend("no matching projects", bot.SendMessage(to, fmt.Sprintf("Nenhum projeto corresponde a %q.", filter)))
		} else {
			logSend("no projects", bot.SendMessage(to, "Sem projetos registados no NucleOS."))
		}
		return
	}
	sendReply(bot, to, "Projetos registados:\n\n"+strings.Join(records, "\n\n"))
}

func HandleCallback(bot Bot, dc Daemon, cb telegram.CallbackQuery) {
	action, id, ok := parseCallback(cb.Data)
	to := telegram.Destination{}
	if cb.Message != nil {
		to = cb.Message.Destination()
	}
	if !ok {
		logSend("unknown callback action", bot.AnswerCallbackQuery(cb.ID, "unknown action"))
		return
	}

	switch action {
	case "approve":
		if _, err := dc.ApproveProposal(id); err != nil {
			logSend("approve failure answer", bot.AnswerCallbackQuery(cb.ID, "approve failed"))
			logSend("approve failure", bot.SendMessage(to, fmt.Sprintf("approve of proposal %d failed: %s", id, err)))
			return
		}
		logSend("approve answer", bot.AnswerCallbackQuery(cb.ID, "approved"))
		logSend("approve confirmation", bot.SendMessage(to, fmt.Sprintf("proposal %d approved", id)))
	case "reject":
		if err := dc.RejectProposal(id); err != nil {
			logSend("reject failure answer", bot.AnswerCallbackQuery(cb.ID, "reject failed"))
			logSend("reject failure", bot.SendMessage(to, fmt.Sprintf("reject of proposal %d failed: %s", id, err)))
			return
		}
		logSend("reject answer", bot.AnswerCallbackQuery(cb.ID, "rejected"))
		logSend("reject confirmation", bot.SendMessage(to, fmt.Sprintf("proposal %d rejected", id)))
	}
}

func RunNotifier(ctx context.Context, bot Bot, dc Daemon, to telegram.Destination, interval time.Duration) {
	state := notifier.NewState()

	// Seeding has to succeed before anything is announced, and at boot the daemon is usually not up
	// yet. Skipping a failed seed meant the first poll that worked treated the whole backlog as
	// new: every pending proposal and the entire feed arriving at once, each proposal under a live
	// Approve button — an invitation to a mis-tap on a decision nobody was making.
	for !seedNotifier(state, dc) {
		if !sleepUntil(ctx, interval) {
			return
		}
	}

	for sleepUntil(ctx, interval) {
		if props, err := dc.GetProposals(); err == nil {
			for _, p := range state.NewProposals(props) {
				err := bot.SendMessageWithButtons(to, "🆕 "+formatProposal(p), approveRejectRow(idOf(p)))
				logSend("new proposal", err)
				if err != nil {
					// Marked as announced before anyone was announced to: without this the prompt
					// to approve an agent's action is lost, and the person deciding never learns
					// there was anything to decide. A duplicate on the next poll is the cheaper
					// mistake — both buttons carry the same proposal id.
					state.Forget(idOf(p))
				}
			}
		}
		if feed, err := dc.GetFeed(); err == nil {
			policy := notifyPolicy(dc)
			// AFTER NewFeedItems, never before, and this is the whole reason the ordering is
			// written down: NewFeedItems marks the entire round as seen — `newItems` rebuilds
			// `stillPresent` from `current` regardless of what it returns — so a suppressed row
			// still enters `seen`. Turning a family back on tomorrow therefore affects future
			// lines only. Filter the feed before this call instead and re-enabling a family dumps
			// ninety days of backlog into the chat at once.
			for _, f := range state.NewFeedItems(feed) {
				if !policy.Allows(kindOf(f)) {
					// A decision, not a failed send — so no state.Forget. Forgetting would offer
					// the row again next round, and it would be declined again for ever.
					continue
				}
				err := bot.SendMessage(to, "📣 "+formatFeed(f))
				logSend("feed item", err)
				if err != nil {
					state.Forget(idOf(f))
				}
			}
		}
		if kill, err := dc.GetKill(); err == nil && state.KillChanged(kill) {
			word := "disengaged"
			if kill {
				word = "ENGAGED"
			}
			err := bot.SendMessage(to, "kill switch "+word)
			logSend("kill switch alert", err)
			if err != nil {
				state.ForgetKill(kill)
			}
		}
		if budget, err := dc.GetBudget(); err == nil && state.BudgetChanged(budget) {
			logSend("budget alert", bot.SendMessage(to, formatBudget(budget)))
		}
	}
}

// notifyPolicy is the owner's selection, or a policy that allows everything when it cannot be
// read.
//
// FAILS OPEN. The failure this whole mechanism exists to prevent is NOISE — and the failure of a mechanism against noise must never be SILENCE, because silence
// is indistinguishable from everything being fine. It is the direction core/src/notify.rs picks
// for `busy_at`, for the same reason.
func notifyPolicy(dc Daemon) notifier.Policy {
	policy, err := dc.GetNotifyPolicy()
	if err != nil {
		log.Printf("notifier: could not read the notification policy; everything passes this round: %v", err)
		return notifier.Policy{}
	}
	return policy
}

// seedNotifier records what already exists so it is never announced. It is all-or-nothing: a
// half-seeded state announces the half it missed.
func seedNotifier(state *notifier.State, dc Daemon) bool {
	props, err := dc.GetProposals()
	if err != nil {
		return false
	}
	feed, err := dc.GetFeed()
	if err != nil {
		return false
	}
	state.NewProposals(props)
	state.NewFeedItems(feed)
	return true
}

// sleepUntil waits out the interval and reports whether the caller should keep going.
func sleepUntil(ctx context.Context, interval time.Duration) bool {
	timer := time.NewTimer(interval)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}

func formatFeed(f map[string]any) string {
	return strOr(f, "kind", "event") + ": " + strOr(f, "summary", "")
}

package pipe

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"nucleostelegram/config"
	"nucleostelegram/format"
	"nucleostelegram/notifier"
	"nucleostelegram/shortcuts"
	"nucleostelegram/telegram"
	"nucleostelegram/transcribe"
)

const pollInterval = 3 * time.Second
const maxPollAttempts = 200

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
	SendMessage(chatID int64, text string) error
	SendHTML(chatID int64, html string) error
	SendMessageWithButtons(chatID int64, text string, rows [][]telegram.Button) error
	AnswerCallbackQuery(callbackID, text string) error
}

type Daemon interface {
	SendAssistantMessage(chatID, text string) (int64, error)
	GetRun(id int64) (map[string]any, error)
	GetProposals() ([]map[string]any, error)
	GetProjects() ([]map[string]any, error)
	ApproveProposal(id int64) (map[string]any, error)
	RejectProposal(id int64) error
	GetFeed() ([]map[string]any, error)
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

type Tracker struct {
	mu       sync.Mutex
	lastTurn map[int64]int64
}

func NewTracker() *Tracker {
	return &Tracker{lastTurn: map[int64]int64{}}
}

func (t *Tracker) Set(chatID, turnID int64) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.lastTurn[chatID] = turnID
}

func (t *Tracker) Get(chatID int64) (int64, bool) {
	t.mu.Lock()
	defer t.mu.Unlock()
	v, ok := t.lastTurn[chatID]
	return v, ok
}

func HandleUpdate(bot Bot, dc Daemon, dl Downloader, cfg config.Config, tr *Tracker, u telegram.Update) {
	if u.CallbackQuery != nil {
		cb := u.CallbackQuery
		if cb.Message == nil || !cfg.IsAllowed(cb.Message.Chat.ID) {
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
	text := resolveIncoming(dl, cfg.TranscribeCmd, u.Message)
	HandleMessage(bot, dc, tr, u.Message.Chat.ID, text)
}

// buildAttachmentPrompt returns the orchestrator prompt for a saved attachment. A document takes
// priority over a photo. Returns ("", false) when neither path is set.
func buildAttachmentPrompt(documentPath, photoPath string) (string, bool) {
	switch {
	case documentPath != "":
		return "The user sent a document. File saved at: " + documentPath, true
	case photoPath != "":
		return "The user sent a photo. File saved at: " + photoPath, true
	default:
		return "", false
	}
}

func downloadToTemp(dl Downloader, fileID, suffix string) (string, error) {
	remotePath, err := dl.GetFile(fileID)
	if err != nil {
		return "", err
	}
	data, err := dl.DownloadFile(remotePath)
	if err != nil {
		return "", err
	}
	f, err := os.CreateTemp("", "nucleos-tg-*"+suffix)
	if err != nil {
		return "", err
	}
	defer f.Close()
	if _, err := f.Write(data); err != nil {
		return "", err
	}
	return f.Name(), nil
}

// resolveIncoming turns a message (possibly a voice note or attachment) into the text to route to
// the orchestrator. Voice → transcript (or a graceful "unavailable" note); document/photo → a
// "file saved at" prompt; otherwise the plain text. Never returns an error — it degrades to a
// human-readable note the orchestrator can relay.
func resolveIncoming(dl Downloader, transcribeCmd string, msg *telegram.Message) string {
	switch {
	case msg.Voice != nil:
		path, err := downloadToTemp(dl, msg.Voice.FileID, ".ogg")
		if err != nil {
			return "[voice message received but could not be downloaded]"
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
		prompt, _ := buildAttachmentPrompt(path, "")
		return prompt
	case len(msg.Photo) > 0:
		// largest photo is the last element in Telegram's ascending-size array
		best := msg.Photo[len(msg.Photo)-1]
		path, err := downloadToTemp(dl, best.FileID, ".jpg")
		if err != nil {
			return "[photo received but could not be downloaded]"
		}
		prompt, _ := buildAttachmentPrompt("", path)
		return prompt
	default:
		return msg.Text
	}
}

func docSuffix(name string) string {
	if i := strings.LastIndex(name, "."); i >= 0 {
		return name[i:]
	}
	return ""
}

func HandleMessage(bot Bot, dc Daemon, tr *Tracker, chatID int64, text string) {
	intent := shortcuts.Route(text)

	switch intent.Kind {
	case shortcuts.Help:
		_ = bot.SendMessage(chatID, helpText)
	case shortcuts.Kill:
		if err := dc.SetKill(intent.On); err != nil {
			_ = bot.SendMessage(chatID, "kill switch error: "+err.Error())
			return
		}
		if intent.On {
			_ = bot.SendMessage(chatID, "kill switch ENGAGED")
		} else {
			_ = bot.SendMessage(chatID, "kill switch disengaged")
		}
	case shortcuts.Cancel:
		id, ok := tr.Get(chatID)
		if !ok {
			_ = bot.SendMessage(chatID, "nothing to cancel")
			return
		}
		if err := dc.CancelRun(id); err != nil {
			_ = bot.SendMessage(chatID, err.Error())
			return
		}
		_ = bot.SendMessage(chatID, fmt.Sprintf("cancelled turn %d", id))
	case shortcuts.Budget:
		budget, err := dc.GetBudget()
		if err != nil {
			_ = bot.SendMessage(chatID, err.Error())
			return
		}
		_ = bot.SendMessage(chatID, formatBudget(budget))
	case shortcuts.Proposals:
		sendProposals(bot, dc, chatID)
	case shortcuts.Proj:
		sendProjects(bot, dc, chatID, intent.Arg)
	case shortcuts.Mail:
		sendTriage(bot, dc, chatID)
	case shortcuts.Inbox:
		sendInbox(bot, dc, chatID)
	default:
		startTurn(bot, dc, tr, chatID, intent.Text)
	}
}

func startTurn(bot Bot, dc Daemon, tr *Tracker, chatID int64, text string) {
	turnID, err := dc.SendAssistantMessage(strconv.FormatInt(chatID, 10), text)
	if err != nil {
		_ = bot.SendMessage(chatID, "couldn't start turn: "+err.Error())
		return
	}
	tr.Set(chatID, turnID)
	_ = bot.SendMessage(chatID, fmt.Sprintf("working… (turn %d)", turnID))
	go func() {
		reply := pollTurnAndReply(dc, turnID, pollInterval, maxPollAttempts)
		sendReply(bot, chatID, reply)
	}()
}

// sendReply renders the orchestrator's Markdown reply to Telegram HTML, splits it under the 4096
// cap, and sends each chunk with parse_mode=HTML; if a chunk is rejected (invalid entities), it
// falls back to plain text so the user always receives the content.
func sendReply(bot Bot, chatID int64, reply string) {
	for _, chunk := range format.Chunk(format.ToHTML(reply), 4096) {
		if err := bot.SendHTML(chatID, chunk); err != nil {
			_ = bot.SendMessage(chatID, format.StripTags(chunk))
		}
	}
}

func pollTurnAndReply(dc Daemon, turnID int64, interval time.Duration, maxAttempts int) string {
	for i := 0; i < maxAttempts; i++ {
		run, err := dc.GetRun(turnID)
		if err != nil {
			return "error checking turn: " + err.Error()
		}
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
	if project, ok := p["project_id"]; ok {
		result += "\nproject: " + fmt.Sprint(project)
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

func sendProposals(bot Bot, dc Daemon, chatID int64) {
	props, err := dc.GetProposals()
	if err != nil {
		_ = bot.SendMessage(chatID, "couldn't fetch proposals: "+err.Error())
		return
	}
	if len(props) == 0 {
		_ = bot.SendMessage(chatID, "no pending proposals")
		return
	}
	for _, p := range props {
		_ = bot.SendMessageWithButtons(chatID, formatProposal(p), approveRejectRow(idOf(p)))
	}
}

// sendTriage spends a run, deliberately and only here: `/mail` is the whole point of the pillar
// being on demand. It answers with what it STARTED, not with verdicts — a run takes minutes, and
// the verdicts arrive by themselves through the feed notifier.
func sendTriage(bot Bot, dc Daemon, chatID int64) {
	outcome, err := dc.TriageEmail()
	if err != nil {
		_ = bot.SendMessage(chatID, "couldn't triage the mailbox: "+err.Error())
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
		_ = bot.SendMessage(chatID, "no triage started: "+reason)
		return
	}
	_ = bot.SendMessage(chatID, fmt.Sprintf(
		"reading %d message(s) — the verdicts arrive here in a few minutes", queued))
}

// sendInbox costs nothing: it reports what is already known, which is what makes it safe to ask
// for at any time.
func sendInbox(bot Bot, dc Daemon, chatID int64) {
	queue, err := dc.GetEmailQueue()
	if err != nil {
		_ = bot.SendMessage(chatID, "couldn't read the mailbox: "+err.Error())
		return
	}
	if len(queue) == 0 {
		_ = bot.SendMessage(chatID, "nothing in the mailbox yet")
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
	_ = bot.SendMessage(chatID, strings.Join(out, "\n"))
}

func sendProjects(bot Bot, dc Daemon, chatID int64, filter string) {
	projects, err := dc.GetProjects()
	if err != nil {
		_ = bot.SendMessage(chatID, "couldn't fetch projects: "+err.Error())
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
			_ = bot.SendMessage(chatID, fmt.Sprintf("Nenhum projeto corresponde a %q.", filter))
		} else {
			_ = bot.SendMessage(chatID, "Sem projetos registados no NucleOS.")
		}
		return
	}
	sendReply(bot, chatID, "Projetos registados:\n\n"+strings.Join(records, "\n\n"))
}

func HandleCallback(bot Bot, dc Daemon, cb telegram.CallbackQuery) {
	action, id, ok := parseCallback(cb.Data)
	chatID := int64(0)
	if cb.Message != nil {
		chatID = cb.Message.Chat.ID
	}
	if !ok {
		_ = bot.AnswerCallbackQuery(cb.ID, "unknown action")
		return
	}

	switch action {
	case "approve":
		if _, err := dc.ApproveProposal(id); err != nil {
			_ = bot.AnswerCallbackQuery(cb.ID, "approve failed")
			_ = bot.SendMessage(chatID, fmt.Sprintf("approve of proposal %d failed: %s", id, err))
			return
		}
		_ = bot.AnswerCallbackQuery(cb.ID, "approved")
		_ = bot.SendMessage(chatID, fmt.Sprintf("proposal %d approved", id))
	case "reject":
		if err := dc.RejectProposal(id); err != nil {
			_ = bot.AnswerCallbackQuery(cb.ID, "reject failed")
			_ = bot.SendMessage(chatID, fmt.Sprintf("reject of proposal %d failed: %s", id, err))
			return
		}
		_ = bot.AnswerCallbackQuery(cb.ID, "rejected")
		_ = bot.SendMessage(chatID, fmt.Sprintf("proposal %d rejected", id))
	}
}

func RunNotifier(bot Bot, dc Daemon, chatID int64, interval time.Duration) {
	state := notifier.NewState()
	if props, err := dc.GetProposals(); err == nil {
		state.NewProposals(props)
	}
	if feed, err := dc.GetFeed(); err == nil {
		state.NewFeedItems(feed)
	}

	for {
		time.Sleep(interval)
		if props, err := dc.GetProposals(); err == nil {
			for _, p := range state.NewProposals(props) {
				_ = bot.SendMessageWithButtons(chatID, "🆕 "+formatProposal(p), approveRejectRow(idOf(p)))
			}
		}
		if feed, err := dc.GetFeed(); err == nil {
			for _, f := range state.NewFeedItems(feed) {
				_ = bot.SendMessage(chatID, "📣 "+formatFeed(f))
			}
		}
		if kill, err := dc.GetKill(); err == nil && state.KillChanged(kill) {
			word := "disengaged"
			if kill {
				word = "ENGAGED"
			}
			_ = bot.SendMessage(chatID, "kill switch "+word)
		}
		if budget, err := dc.GetBudget(); err == nil && state.BudgetChanged(budget) {
			_ = bot.SendMessage(chatID, formatBudget(budget))
		}
	}
}

func formatFeed(f map[string]any) string {
	return strOr(f, "kind", "event") + ": " + strOr(f, "summary", "")
}

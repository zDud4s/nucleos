package pipe

import (
	"fmt"
	"log"
	"regexp"
	"strconv"
	"strings"

	"nucleostelegram/telegram"
)

// captureTypedOnly refuses a dictated or attached answer: capture answers are typed (spec P6),
// for the same reason a spoken command is never run.
const captureTypedOnly = "Capture requests take a typed reply — type your answer instead."

// captureMarkPattern finds the #cap<job_id> mark the núcleo puts at the end of every capture
// request. The mark travels in the message itself, so the sidecar keeps no state for it.
var captureMarkPattern = regexp.MustCompile(`#cap(\d+)\b`)

// captureMark returns the job id a capture request's text names, or 0 when it names none. The
// LAST mark wins: the núcleo writes it at the very end, after facts that quote free text (an
// item's description) which could itself contain "#cap<N>".
func captureMark(text string) int64 {
	all := captureMarkPattern.FindAllStringSubmatch(text, -1)
	if len(all) == 0 {
		return 0
	}
	id, err := strconv.ParseInt(all[len(all)-1][1], 10, 64)
	if err != nil {
		return 0
	}
	return id
}

// captureTarget is the job a message answers: a reply to a message a bot wrote that carries a
// #cap mark. is_bot rather than "this bot" because the sidecar never calls getMe; in an authorised
// chat only this bot emits the mark, and the núcleo still answers 404 for a job with no request.
func captureTarget(msg *telegram.Message) int64 {
	to := msg.ReplyToMessage
	if to == nil || to.From == nil || !to.From.IsBot {
		return 0
	}
	return captureMark(to.Text)
}

// hintOpenCaptures tells the owner, once per open capture request, that a loose message is not an
// answer: only a Reply to the request carries its #cap mark. The message itself still goes where it
// was going — guessing it was meant as the answer could file a conversation as a job's note. A
// command is left alone, and a daemon that cannot list the requests costs only the hint.
func hintOpenCaptures(bot Bot, dc Daemon, tr *Tracker, to telegram.Destination, msg *telegram.Message) {
	if strings.HasPrefix(strings.TrimSpace(msg.Text), "/") {
		return
	}
	jobs, err := dc.OpenCaptureJobs()
	if err != nil {
		log.Printf("capture hint skipped: %v", err)
		return
	}
	fresh := tr.unhinted(jobs)
	if len(fresh) == 0 {
		return
	}
	logSend("capture hint", bot.SendMessage(to, captureHint(fresh)))
}

// captureHint is the text of the hint for the given jobs.
func captureHint(jobs []int64) string {
	marks := make([]string, len(jobs))
	for i, job := range jobs {
		marks[i] = fmt.Sprintf("#cap%d", job)
	}
	return "This message went to the assistant, not to a capture request. To answer one, use Reply on its message (" +
		strings.Join(marks, ", ") + ")."
}

// handleCaptureReply answers a capture request. It never opens an assistant turn, and the
// answer's text is never logged: it is the owner's private writing, like a note.
func handleCaptureReply(bot Bot, dc Daemon, to telegram.Destination, msg *telegram.Message, jobID int64) {
	if msg.Voice != nil || len(msg.Photo) > 0 || msg.Document != nil {
		logSend("capture reply refused", bot.SendMessage(to, captureTypedOnly))
		return
	}
	// A sticker, a location or a poll has no text either: never an empty note.
	if strings.TrimSpace(msg.Text) == "" {
		logSend("capture reply unreadable", bot.SendMessage(to, captureTypedOnly))
		return
	}
	noteID, released, err := dc.AnswerCapture(jobID, msg.Text)
	if err != nil {
		logSend("capture answer error", bot.SendMessage(to, "couldn't save the answer: "+err.Error()))
		return
	}
	if !released {
		logSend("capture answer late", bot.SendMessage(to,
			fmt.Sprintf("noted (#%d); the distiller already moved on for this job", noteID)))
		return
	}
	logSend("capture answered", bot.SendMessage(to, fmt.Sprintf("noted (#%d), linked to job #%d", noteID, jobID)))
}

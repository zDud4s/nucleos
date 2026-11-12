package adapter

import (
	"bytes"
	"context"
	"errors"
	"io/fs"
	"log"
	"strconv"
	"strings"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
	"nucleoschannelkit/kit"

	"nucleostelegram/telegram"
)

// fakeSendBot records every SendHTMLEx call; errs[i] is the error of the i-th call (nil = success).
type fakeSendBot struct {
	calls []telegram.SendParams
	errs  map[int]error
}

func (f *fakeSendBot) SendHTMLEx(_ context.Context, p telegram.SendParams) (int64, error) {
	i := len(f.calls)
	f.calls = append(f.calls, p)
	if err := f.errs[i]; err != nil {
		return 0, err
	}
	return int64(1000 + i), nil
}

func loadSend(t *testing.T, name string) contract.Send {
	t.Helper()
	b, err := fs.ReadFile(fixtures.FS(), "outbox/"+name+".json")
	if err != nil {
		t.Fatal(err)
	}
	a, err := contract.ParseAction(b)
	if err != nil {
		t.Fatal(err)
	}
	return a.(contract.Send)
}

func apiErr(status int, desc string) error {
	return &telegram.APIError{Method: "sendMessage", StatusCode: status, Description: desc}
}

func TestExecuteSendText(t *testing.T) {
	bot := &fakeSendBot{}
	refs, err := New(bot, nil).Execute(context.Background(), loadSend(t, "send-text"))
	if err != nil {
		t.Fatal(err)
	}
	if len(bot.calls) != 1 || len(refs) != 1 || refs[0] != "1000" {
		t.Fatalf("calls=%d refs=%v", len(bot.calls), refs)
	}
	p := bot.calls[0]
	if p.To != (telegram.Destination{ChatID: 4242}) || p.Text != "Done." || p.ParseMode != "HTML" {
		t.Fatalf("params = %+v", p)
	}
}

func TestExecuteSendFull(t *testing.T) {
	s := loadSend(t, "send-full")
	// Media parts belong to B46: keep only the text part here.
	var parts []contract.OutPart
	for _, p := range s.Parts {
		if p.Type == contract.OutText {
			parts = append(parts, p)
		}
	}
	s.Parts = parts
	bot := &fakeSendBot{}
	refs, err := New(bot, nil).Execute(context.Background(), s)
	if err != nil {
		t.Fatal(err)
	}
	if len(bot.calls) != 1 || len(refs) != 1 {
		t.Fatalf("calls=%d refs=%v", len(bot.calls), refs)
	}
	p := bot.calls[0]
	if p.To != (telegram.Destination{ChatID: -100123, ThreadID: 5}) {
		t.Fatalf("destination = %+v", p.To)
	}
	if p.ReplyToMessageID != 980 || !p.Silent {
		t.Fatalf("reply=%d silent=%v", p.ReplyToMessageID, p.Silent)
	}
	if len(p.Buttons) != 1 || len(p.Buttons[0]) != 2 ||
		p.Buttons[0][0] != (telegram.Button{Text: "Approve", CallbackData: "tk_9f2c"}) ||
		p.Buttons[0][1] != (telegram.Button{Text: "Reject", CallbackData: "tk_9f2d"}) {
		t.Fatalf("buttons = %+v", p.Buttons)
	}
	if !strings.Contains(p.Text, "<b>Proposal 12</b>") {
		t.Fatalf("text not rendered to HTML: %q", p.Text)
	}
}

func TestExecuteSendLongTextButtonsOnLastPieceOnly(t *testing.T) {
	long := strings.Repeat("word ", 2000) // 10 000 characters
	reply := uint64(7)
	s := contract.Send{
		ID: 1, Conversation: "4242", ReplyTo: &reply, ReplyToPlatformRef: "55",
		Parts:   []contract.OutPart{{Type: contract.OutText, Text: long}},
		Buttons: [][]contract.Button{{{Label: "Ok", Token: "tk_1"}}},
	}
	bot := &fakeSendBot{}
	refs, err := New(bot, nil).Execute(context.Background(), s)
	if err != nil {
		t.Fatal(err)
	}
	n := len(bot.calls)
	if n < 3 || len(refs) != n {
		t.Fatalf("calls=%d refs=%v", n, refs)
	}
	for i, p := range bot.calls {
		if len([]rune(p.Text)) > 4096 {
			t.Errorf("piece %d has %d runes", i, len([]rune(p.Text)))
		}
		if (i == 0) != (p.ReplyToMessageID == 55) {
			t.Errorf("piece %d reply_to = %d", i, p.ReplyToMessageID)
		}
		if (i == n-1) != (len(p.Buttons) > 0) {
			t.Errorf("piece %d buttons = %+v", i, p.Buttons)
		}
		if refs[i] != strconv.Itoa(1000+i) {
			t.Errorf("ref %d = %q", i, refs[i])
		}
	}
}

func TestExecuteSendRateLimited(t *testing.T) {
	bot := &fakeSendBot{errs: map[int]error{0: &telegram.APIError{Method: "sendMessage", StatusCode: 429, RetryAfter: 45 * time.Second}}}
	_, err := New(bot, nil).Execute(context.Background(), loadSend(t, "send-text"))
	var ee *kit.ExecError
	if !errors.As(err, &ee) || !ee.Retryable || ee.Reason != "rate_limited" || ee.RetryAfter != 45*time.Second {
		t.Fatalf("err = %#v", err)
	}
}

func TestExecuteSendThreadNotFound(t *testing.T) {
	bot := &fakeSendBot{errs: map[int]error{0: apiErr(400, "Bad Request: message thread not found")}}
	_, err := New(bot, nil).Execute(context.Background(), loadSend(t, "send-text"))
	var ee *kit.ExecError
	if !errors.As(err, &ee) || ee.Retryable || ee.Reason != "thread_not_found" {
		t.Fatalf("err = %#v", err)
	}
}

func TestExecuteSendParseEntitiesFallsBackToPlain(t *testing.T) {
	s := contract.Send{ID: 2, Conversation: "4242", Parts: []contract.OutPart{{Type: contract.OutText, Text: "**bold** text"}}}
	bot := &fakeSendBot{errs: map[int]error{0: apiErr(400, "Bad Request: can't parse entities: Can't find end of the entity")}}
	refs, err := New(bot, nil).Execute(context.Background(), s)
	if err != nil {
		t.Fatal(err)
	}
	if len(bot.calls) != 2 || len(refs) != 1 || refs[0] != "1001" {
		t.Fatalf("calls=%d refs=%v", len(bot.calls), refs)
	}
	if bot.calls[0].ParseMode != "HTML" || bot.calls[1].ParseMode != "" || bot.calls[1].Text != "bold text" {
		t.Fatalf("calls = %+v", bot.calls)
	}
}

func TestExecuteSendMidFailureLogsSentRefsAndAcksNone(t *testing.T) {
	var logs bytes.Buffer
	log.SetOutput(&logs)
	defer log.SetOutput(log.Writer())

	s := contract.Send{ID: 3, Conversation: "4242", Parts: []contract.OutPart{
		{Type: contract.OutText, Text: "one"}, {Type: contract.OutText, Text: "two"}, {Type: contract.OutText, Text: "three"}}}
	bot := &fakeSendBot{errs: map[int]error{1: apiErr(502, "Bad Gateway")}}
	refs, err := New(bot, nil).Execute(context.Background(), s)
	var ee *kit.ExecError
	if !errors.As(err, &ee) || !ee.Retryable || ee.Reason != "telegram_5xx" {
		t.Fatalf("err = %#v", err)
	}
	if refs != nil {
		t.Fatalf("partial refs leaked into the result: %v", refs)
	}
	if len(bot.calls) != 2 {
		t.Fatalf("calls = %d, want the send to stop at the failure", len(bot.calls))
	}
	if !strings.Contains(logs.String(), "1000") {
		t.Fatalf("sent ref not logged: %q", logs.String())
	}
}

func TestExecuteSendMalformedConversation(t *testing.T) {
	for _, conv := range []string{"abc", "12:x", "1:2:3", ""} {
		s := contract.Send{ID: 4, Conversation: conv, Parts: []contract.OutPart{{Type: contract.OutText, Text: "x"}}}
		bot := &fakeSendBot{}
		_, err := New(bot, nil).Execute(context.Background(), s)
		var ee *kit.ExecError
		if !errors.As(err, &ee) || ee.Retryable || ee.Reason != "invalid_action" || len(bot.calls) != 0 {
			t.Errorf("conversation %q: err=%#v calls=%d", conv, err, len(bot.calls))
		}
	}
}

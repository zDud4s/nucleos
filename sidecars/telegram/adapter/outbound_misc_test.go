package adapter

import (
	"context"
	"errors"
	"io/fs"
	"net/url"
	"strings"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
	"nucleoschannelkit/kit"

	"nucleostelegram/telegram"
)

// fakeSendBot (outbound_test.go) only records sends; these no-ops make it satisfy SendBot, and
// fakeMiscBot below shadows them with recording versions.
func (f *fakeSendBot) EditMessageText(context.Context, int64, int64, string, string, [][]telegram.Button) error {
	return nil
}

func (f *fakeSendBot) SendChatAction(context.Context, telegram.Destination, string) error {
	return nil
}

func (f *fakeSendBot) AnswerCallbackQueryCtx(context.Context, string, string) error { return nil }

type editCall struct {
	chatID, messageID int64
	text, parseMode   string
	buttons           [][]telegram.Button
}

type actionCall struct {
	to     telegram.Destination
	action string
}

type answerCall struct{ id, text string }

// fakeMiscBot extends fakeSendBot with the edit, chat-action and callback-answer calls.
type fakeMiscBot struct {
	fakeSendBot
	edits     []editCall
	actions   []actionCall
	answers   []answerCall
	editErrs  map[int]error
	actionErr error
	answerErr error
}

func (f *fakeMiscBot) EditMessageText(_ context.Context, chatID, messageID int64, text, parseMode string, buttons [][]telegram.Button) error {
	i := len(f.edits)
	f.edits = append(f.edits, editCall{chatID, messageID, text, parseMode, buttons})
	return f.editErrs[i]
}

func (f *fakeMiscBot) SendChatAction(_ context.Context, to telegram.Destination, action string) error {
	f.actions = append(f.actions, actionCall{to, action})
	return f.actionErr
}

func (f *fakeMiscBot) AnswerCallbackQueryCtx(_ context.Context, id, text string) error {
	f.answers = append(f.answers, answerCall{id, text})
	return f.answerErr
}

func loadAction(t *testing.T, name string) contract.Action {
	t.Helper()
	b, err := fs.ReadFile(fixtures.FS(), "outbox/"+name+".json")
	if err != nil {
		t.Fatal(err)
	}
	a, err := contract.ParseAction(b)
	if err != nil {
		t.Fatal(err)
	}
	return a
}

func execError(t *testing.T, err error) *kit.ExecError {
	t.Helper()
	var ee *kit.ExecError
	if !errors.As(err, &ee) {
		t.Fatalf("err = %#v, want *kit.ExecError", err)
	}
	return ee
}

func editWith(text string, refs ...string) contract.Edit {
	return contract.Edit{
		ID: 1, Conversation: "-100123:5", Target: 7, TargetPlatformRefs: refs,
		Parts: []contract.OutPart{{Type: contract.OutText, Text: text}},
	}
}

func TestExecuteEditFixtureShape(t *testing.T) {
	// The fixture has one piece over two refs: the shape does not match.
	bot := &fakeMiscBot{}
	_, err := New(bot, nil).Execute(context.Background(), loadAction(t, "edit"))
	if ee := execError(t, err); ee.Retryable || ee.Reason != "edit_unsupported_shape" {
		t.Fatalf("err = %#v", ee)
	}
	if len(bot.edits) != 0 {
		t.Fatalf("editMessageText called: %+v", bot.edits)
	}
}

func TestExecuteEditShape(t *testing.T) {
	long := strings.Repeat("word ", 2000)
	cases := map[string]contract.Edit{
		"two pieces over one ref": editWith(long, "993"),
		"one piece over two refs": editWith("short", "993", "994"),
	}
	for name, e := range cases {
		t.Run(name, func(t *testing.T) {
			bot := &fakeMiscBot{}
			_, err := New(bot, nil).Execute(context.Background(), e)
			if ee := execError(t, err); ee.Retryable || ee.Reason != "edit_unsupported_shape" {
				t.Fatalf("err = %#v", ee)
			}
			if len(bot.edits) != 0 {
				t.Fatalf("editMessageText called: %+v", bot.edits)
			}
		})
	}
}

func TestExecuteEditFirstRef(t *testing.T) {
	e := editWith("**Proposal 12**: approved", "993")
	e.Buttons = [][]contract.Button{{{Label: "Ok", Token: "tk_1"}}}
	bot := &fakeMiscBot{}
	refs, err := New(bot, nil).Execute(context.Background(), e)
	if err != nil {
		t.Fatal(err)
	}
	if len(refs) != 1 || refs[0] != "993" {
		t.Fatalf("refs = %v", refs)
	}
	if len(bot.edits) != 1 {
		t.Fatalf("edits = %+v", bot.edits)
	}
	c := bot.edits[0]
	if c.chatID != -100123 || c.messageID != 993 || c.parseMode != "HTML" || c.text != "<b>Proposal 12</b>: approved" {
		t.Fatalf("edit = %+v", c)
	}
	if len(c.buttons) != 1 || c.buttons[0][0] != (telegram.Button{Text: "Ok", CallbackData: "tk_1"}) {
		t.Fatalf("buttons = %+v", c.buttons)
	}
	if len(bot.calls) != 0 {
		t.Fatalf("sendMessage called on edit")
	}
}

func TestExecuteEditNotModified(t *testing.T) {
	bot := &fakeMiscBot{editErrs: map[int]error{0: apiErr(400, "Bad Request: message is not modified: specified new message content and reply markup are exactly the same")}}
	refs, err := New(bot, nil).Execute(context.Background(), editWith("same", "993"))
	if err != nil {
		t.Fatal(err)
	}
	if len(refs) != 1 || refs[0] != "993" {
		t.Fatalf("refs = %v", refs)
	}
}

func TestExecuteEditParseFallback(t *testing.T) {
	bot := &fakeMiscBot{editErrs: map[int]error{0: apiErr(400, "Bad Request: can't parse entities: bad")}}
	refs, err := New(bot, nil).Execute(context.Background(), editWith("**hi**", "993"))
	if err != nil || len(refs) != 1 {
		t.Fatalf("refs=%v err=%v", refs, err)
	}
	if len(bot.edits) != 2 || bot.edits[1].parseMode != "" || bot.edits[1].text != "hi" {
		t.Fatalf("edits = %+v", bot.edits)
	}
}

func TestExecuteEditNetworkRetryable(t *testing.T) {
	bot := &fakeMiscBot{editErrs: map[int]error{0: &url.Error{Op: "Post", URL: "x", Err: errors.New("refused")}}}
	_, err := New(bot, nil).Execute(context.Background(), editWith("x", "993"))
	if ee := execError(t, err); !ee.Retryable || ee.Reason != "network" {
		t.Fatalf("err = %#v", ee)
	}
}

func TestExecuteEditInvalid(t *testing.T) {
	bot := &fakeMiscBot{}
	for _, e := range []contract.Edit{editWith("x"), editWith("x", "abc")} {
		_, err := New(bot, nil).Execute(context.Background(), e)
		if ee := execError(t, err); ee.Retryable || ee.Reason != "invalid_action" {
			t.Fatalf("err = %#v", ee)
		}
	}
}

func TestAnswerFixture(t *testing.T) {
	bot := &fakeMiscBot{}
	refs, err := New(bot, nil).Execute(context.Background(), loadAction(t, "answer-action"))
	if err != nil {
		t.Fatal(err)
	}
	if len(refs) != 0 {
		t.Fatalf("refs = %v, want none", refs)
	}
	if len(bot.answers) != 1 || bot.answers[0] != (answerCall{"551", "Already handled."}) {
		t.Fatalf("answers = %+v", bot.answers)
	}
}

func TestAnswerQueryTooOldIsOK(t *testing.T) {
	bot := &fakeMiscBot{answerErr: apiErr(400, "Bad Request: query is too old and response timeout expired or query ID is invalid")}
	refs, err := New(bot, nil).Execute(context.Background(), loadAction(t, "answer-action"))
	if err != nil || len(refs) != 0 {
		t.Fatalf("refs=%v err=%v", refs, err)
	}
}

func TestAnswerNetworkRetryable(t *testing.T) {
	bot := &fakeMiscBot{answerErr: &url.Error{Op: "Post", URL: "x", Err: errors.New("refused")}}
	_, err := New(bot, nil).Execute(context.Background(), loadAction(t, "answer-action"))
	if ee := execError(t, err); !ee.Retryable || ee.Reason != "network" {
		t.Fatalf("err = %#v", ee)
	}
}

func TestAnswerOtherErrorPermanent(t *testing.T) {
	bot := &fakeMiscBot{answerErr: apiErr(400, "Bad Request: something else")}
	_, err := New(bot, nil).Execute(context.Background(), loadAction(t, "answer-action"))
	if ee := execError(t, err); ee.Retryable || ee.Reason != "invalid_action" {
		t.Fatalf("err = %#v", ee)
	}
}

func TestTypingSendsChatAction(t *testing.T) {
	bot := &fakeMiscBot{}
	if err := New(bot, nil).Typing(context.Background(), "-100123:5"); err != nil {
		t.Fatal(err)
	}
	want := actionCall{telegram.Destination{ChatID: -100123, ThreadID: 5}, "typing"}
	if len(bot.actions) != 1 || bot.actions[0] != want {
		t.Fatalf("actions = %+v", bot.actions)
	}
}

func TestTypingErrorsReturned(t *testing.T) {
	bot := &fakeMiscBot{actionErr: apiErr(403, "Forbidden: bot was blocked")}
	if err := New(bot, nil).Typing(context.Background(), "4242"); err == nil {
		t.Fatal("want error")
	}
	if err := New(&fakeMiscBot{}, nil).Typing(context.Background(), "nope"); err == nil {
		t.Fatal("want error for bad conversation")
	}
}

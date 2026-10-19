package contract_test

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"

	"nucleoschannelkit/contract"
)

func init() {
	register("action", func(b []byte) (any, error) {
		v, err := contract.ParseAction(b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
}

func wantSendErr(t *testing.T, doc string, code contract.ErrorCode, at string) {
	t.Helper()
	_, err := contract.ParseAction([]byte(doc))
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != code || ce.At != at {
		t.Fatalf("%s: err = %v, want %s at %q", doc, err, code, at)
	}
}

func TestSendPartsKeepTheirOrderAndKinds(t *testing.T) {
	a, err := contract.ParseAction(readFixture(t, "outbox/send-full.json"))
	if err != nil {
		t.Fatal(err)
	}
	s, ok := a.(contract.Send)
	if !ok {
		t.Fatalf("got %T, want contract.Send", a)
	}
	want := []contract.OutPartType{contract.OutText, contract.OutImage, contract.OutFile, contract.OutAudio}
	if len(s.Parts) != len(want) {
		t.Fatalf("parts = %d", len(s.Parts))
	}
	for i, p := range s.Parts {
		if p.Type != want[i] {
			t.Errorf("part %d type = %s, want %s", i, p.Type, want[i])
		}
	}
	if s.Parts[1].Caption != "diff" || s.Parts[2].Name != "relatorio.pdf" || s.Parts[3].Mime != "audio/ogg" {
		t.Errorf("part fields lost: %+v", s.Parts)
	}
}

func TestSendRepliesButtonsAndSilent(t *testing.T) {
	a, err := contract.ParseAction(readFixture(t, "outbox/send-full.json"))
	if err != nil {
		t.Fatal(err)
	}
	s := a.(contract.Send)
	if s.ID != 57 || s.Conversation != "-100123:5" {
		t.Errorf("id/conversation = %d %q", s.ID, s.Conversation)
	}
	if s.ReplyTo == nil || *s.ReplyTo != 52 || s.ReplyToPlatformRef != "980" {
		t.Errorf("reply = %v %q", s.ReplyTo, s.ReplyToPlatformRef)
	}
	if !s.Silent {
		t.Error("silent lost")
	}
	if len(s.Buttons) != 1 || len(s.Buttons[0]) != 2 || s.Buttons[0][1] != (contract.Button{Label: "Reject", Token: "tk_9f2d"}) {
		t.Errorf("buttons = %+v", s.Buttons)
	}
}

func TestSendDefaultsAreOmittedWhenEncoding(t *testing.T) {
	a, err := contract.ParseAction(readFixture(t, "outbox/send-text.json"))
	if err != nil {
		t.Fatal(err)
	}
	b, err := json.Marshal(a)
	if err != nil {
		t.Fatal(err)
	}
	if got := string(b); got != `{"id":41,"type":"send","conversation":"4242","parts":[{"type":"text","text":"Done."}]}` {
		t.Errorf("got %s", got)
	}
}

func TestSendIDMustBePositive(t *testing.T) {
	wantSendErr(t, `{"id":0,"type":"send","conversation":"c","parts":[{"type":"text","text":"x"}]}`, contract.CodeRange, "/id")
	_, err := contract.ParseAction([]byte(`{"id":-1,"type":"send","conversation":"c","parts":[{"type":"text","text":"x"}]}`))
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != contract.CodeDecode {
		t.Fatalf("negative id: %v", err)
	}
	wantSendErr(t, `{"type":"send","conversation":"c","parts":[{"type":"text","text":"x"}]}`, contract.CodeDecode, "/id")
}

func TestSendButtonTokenGrammar(t *testing.T) {
	long := "tk_" + strings.Repeat("a", 62)
	doc := `{"id":1,"type":"send","conversation":"c","parts":[{"type":"text","text":"x"}],"buttons":[[{"label":"l","token":"` + long + `"}]]}`
	wantSendErr(t, doc, contract.CodeGrammar, "/buttons/0/0/token")
	ok := `{"id":1,"type":"send","conversation":"c","parts":[{"type":"text","text":"x"}],"buttons":[[{"label":"l","token":"tk_` + strings.Repeat("a", 61) + `"}]]}`
	if _, err := contract.ParseAction([]byte(ok)); err != nil {
		t.Fatalf("61-char token body must be valid: %v", err)
	}
}

func TestSendReplyToRules(t *testing.T) {
	wantSendErr(t, `{"id":1,"type":"send","conversation":"c","reply_to":0,"parts":[{"type":"text","text":"x"}]}`, contract.CodeRange, "/reply_to")
	wantSendErr(t, `{"id":1,"type":"send","conversation":"c","reply_to":2,"reply_to_platform_ref":"","parts":[{"type":"text","text":"x"}]}`, contract.CodeGrammar, "/reply_to_platform_ref")
}

func TestSendAudioAndFileMime(t *testing.T) {
	wantSendErr(t, `{"id":1,"type":"send","conversation":"c","parts":[{"type":"audio","media":"m_1","mime":"audio/ogg; codecs=opus"}]}`, contract.CodeGrammar, "/parts/0/mime")
	wantSendErr(t, `{"id":1,"type":"send","conversation":"c","parts":[{"type":"image","media":"m_1","mime":"image/png"}]}`, contract.CodeForbidden, "/parts/0/mime")
	wantSendErr(t, `{"id":1,"type":"send","conversation":"c","parts":[{"type":"audio","media":"m_1","name":"a.ogg"}]}`, contract.CodeForbidden, "/parts/0/name")
}

func TestSendTextPartForbidsMedia(t *testing.T) {
	wantSendErr(t, `{"id":1,"type":"send","conversation":"c","parts":[{"type":"text","text":"x","media":"m_1"}]}`, contract.CodeForbidden, "/parts/0/media")
}

func TestSendOtherTypesAreNotYetDecoded(t *testing.T) {
	for _, doc := range []string{
		`{"id":1,"type":"edit","conversation":"c","target":1,"target_platform_refs":["1"],"parts":[{"type":"text","text":"x"}]}`,
		`{"id":1,"type":"answer_action","conversation":"c","action_ref":"a","text":"x"}`,
		`{"id":1,"type":"react","conversation":"c"}`,
	} {
		_, err := contract.ParseAction([]byte(doc))
		var ce *contract.Error
		if !errors.As(err, &ce) || ce.Code != contract.CodeDecode {
			t.Errorf("%s: err = %v", doc, err)
		}
	}
}

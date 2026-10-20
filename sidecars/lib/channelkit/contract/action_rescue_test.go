package contract_test

import (
	"encoding/json"
	"errors"
	"path"
	"reflect"
	"strings"
	"testing"

	"nucleoschannelkit/contract"
)

func TestDecodeActionRescueAckForEveryInvalidFixture(t *testing.T) {
	n := 0
	for _, e := range loadIndex(t).Fixtures {
		if e.Shape != "action" || e.Go == nil || !e.Go.FallbackAck.Set {
			continue
		}
		if valid, _ := effective(e); valid {
			continue
		}
		n++
		t.Run(strings.TrimSuffix(path.Base(e.File), ".json"), func(t *testing.T) {
			a, ack, err := contract.DecodeAction(readFixture(t, e.File))
			if err == nil || a != nil {
				t.Fatalf("got %v, %v; want an error and no action", a, err)
			}
			if e.Go.FallbackAck.Value == nil {
				if ack != nil {
					t.Fatalf("ack = %+v, want none", ack)
				}
				return
			}
			if ack == nil {
				t.Fatal("no rescue ack")
			}
			got, merr := json.Marshal(ack)
			if merr != nil {
				t.Fatal(merr)
			}
			gotV, _ := parseValue(got)
			wantV, _ := parseValue([]byte(`{"failed":{"retryable":false,"reason":"` + *e.Go.FallbackAck.Value + `"}}`))
			if !reflect.DeepEqual(gotV, wantV) {
				t.Errorf("ack = %s, want reason %s", got, *e.Go.FallbackAck.Value)
			}
		})
	}
	if n < 10 {
		t.Fatalf("only %d invalid action fixtures walked", n)
	}
}

func TestDecodeActionValidReturnsNoAck(t *testing.T) {
	a, ack, err := contract.DecodeAction(readFixture(t, "outbox/edit.json"))
	if err != nil || ack != nil {
		t.Fatalf("err = %v, ack = %v", err, ack)
	}
	e, ok := a.(contract.Edit)
	if !ok || e.Target != 57 || len(e.TargetPlatformRefs) != 2 || e.ID != 58 {
		t.Fatalf("got %#v", a)
	}
}

func TestDecodeActionAnswer(t *testing.T) {
	a, _, err := contract.DecodeAction(readFixture(t, "outbox/answer-action.json"))
	if err != nil {
		t.Fatal(err)
	}
	if got := a.(contract.AnswerAction); got.ActionRef != "551" || got.Text != "Already handled." || got.ID != 59 {
		t.Fatalf("got %+v", got)
	}
}

func TestDecodeActionWithoutUsableIDHasNoAck(t *testing.T) {
	for _, doc := range []string{
		`not json`,
		`{"type":"send","conversation":"c","parts":[{"type":"text","text":"x"}]}`,
		`{"id":-3,"type":"send","conversation":"c"}`,
		`{"id":"7","type":"send","conversation":"c"}`,
		`{"id":1.5,"type":"send","conversation":"c"}`,
		`[1]`,
	} {
		a, ack, err := contract.DecodeAction([]byte(doc))
		if err == nil || a != nil || ack != nil {
			t.Errorf("%s: a=%v ack=%v err=%v", doc, a, ack, err)
		}
	}
}

func TestDecodeActionReasonByCause(t *testing.T) {
	cases := map[string]string{
		`{"id":5,"conversation":"c"}`:                                         contract.ReasonInvalidAction,
		`{"id":5,"type":7,"conversation":"c"}`:                                contract.ReasonInvalidAction,
		`{"id":5,"type":"react","conversation":"c"}`:                          contract.ReasonUnsupportedAction,
		`{"id":5,"type":"edit","conversation":"c"}`:                           contract.ReasonInvalidAction,
		`{"id":5,"type":"answer_action","conversation":"c","action_ref":"a"}`: contract.ReasonInvalidAction,
	}
	for doc, want := range cases {
		_, ack, err := contract.DecodeAction([]byte(doc))
		var ce *contract.Error
		if !errors.As(err, &ce) {
			t.Errorf("%s: err = %v", doc, err)
		}
		if ack == nil || ack.Failed == nil || ack.Failed.Retryable || ack.Failed.Reason != want {
			t.Errorf("%s: ack = %+v, want %s", doc, ack, want)
		}
	}
}

func TestEditRules(t *testing.T) {
	const parts = `"parts":[{"type":"text","text":"x"}]`
	wantSendErr(t, `{"id":1,"type":"edit","conversation":"c","target":0,"target_platform_refs":["1"],`+parts+`}`, contract.CodeRange, "/target")
	wantSendErr(t, `{"id":1,"type":"edit","conversation":"c","target_platform_refs":["1"],`+parts+`}`, contract.CodeRequiredIf, "/target")
	wantSendErr(t, `{"id":1,"type":"edit","conversation":"c","target":2,"target_platform_refs":[],`+parts+`}`, contract.CodeEmpty, "/target_platform_refs")
	wantSendErr(t, `{"id":1,"type":"edit","conversation":"c","target":2,"target_platform_refs":["1",""],`+parts+`}`, contract.CodeEmpty, "/target_platform_refs/1")
	wantSendErr(t, `{"id":1,"type":"edit","conversation":"c","target":2,"target_platform_refs":["1"],"reply_to_platform_ref":"9",`+parts+`}`, contract.CodeForbidden, "/reply_to_platform_ref")
	wantSendErr(t, `{"id":1,"type":"edit","conversation":"c","target":2,"target_platform_refs":["1"],"parts":[]}`, contract.CodeEmpty, "/parts")
	wantSendErr(t, `{"id":1,"type":"edit","conversation":"c","target":2,"target_platform_refs":["1"]}`, contract.CodeDecode, "/parts")
	ok := `{"id":1,"type":"edit","conversation":"c","target":2,"target_platform_refs":["1"],` + parts + `,"buttons":[[{"label":"l","token":"tk_a"}]]}`
	a, err := contract.ParseAction([]byte(ok))
	if err != nil || len(a.(contract.Edit).Buttons) != 1 {
		t.Fatalf("edit with buttons: %v %v", a, err)
	}
}

func TestAnswerActionRules(t *testing.T) {
	wantSendErr(t, `{"id":1,"type":"answer_action","conversation":"c","text":"x"}`, contract.CodeRequiredIf, "/action_ref")
	wantSendErr(t, `{"id":1,"type":"answer_action","conversation":"c","action_ref":"a"}`, contract.CodeRequiredIf, "/text")
	wantSendErr(t, `{"id":1,"type":"answer_action","conversation":"c","action_ref":"a","text":"x","parts":[{"type":"text","text":"x"}]}`, contract.CodeForbidden, "/parts")
	wantSendErr(t, `{"id":1,"type":"send","conversation":"c","target":3,"parts":[{"type":"text","text":"x"}]}`, contract.CodeForbidden, "/target")
	if _, err := contract.ParseAction([]byte(`{"id":1,"type":"answer_action","conversation":"c","action_ref":"a","text":""}`)); err != nil {
		t.Fatalf("empty text is allowed: %v", err)
	}
}

func TestLane(t *testing.T) {
	cases := map[string]contract.Lane{
		"outbox/send-text.json":     contract.Ordered,
		"outbox/edit.json":          contract.Ordered,
		"outbox/answer-action.json": contract.Immediate,
	}
	for file, want := range cases {
		a, err := contract.ParseAction(readFixture(t, file))
		if err != nil {
			t.Fatal(err)
		}
		if got := a.Lane(); got != want {
			t.Errorf("%s: lane = %s, want %s", file, got, want)
		}
	}
}

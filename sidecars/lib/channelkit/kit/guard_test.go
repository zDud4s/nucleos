package kit

import (
	"reflect"
	"strings"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

func guardCaps(mutate func(*contract.Capabilities)) contract.Capabilities {
	mt := 100
	c := contract.Capabilities{MaxText: &mt}
	if mutate != nil {
		mutate(&c)
	}
	return c
}

func guardText(s string) []contract.OutPart {
	return []contract.OutPart{{Type: contract.OutText, Text: s}}
}

func guardMedia(kind contract.OutPartType) []contract.OutPart {
	p := contract.OutPart{Type: kind, Media: "m1"}
	if kind == contract.OutFile {
		p.Name = "a.bin"
	}
	return []contract.OutPart{p}
}

func guardButtons() [][]contract.Button {
	return [][]contract.Button{{{Label: "Yes", Token: "tk_1"}}}
}

func TestGuardRejectsWhatTheHelloDidNotDeclare(t *testing.T) {
	want := ackFixture(t, "unsupported-capability.json")
	one := uint64(1)
	cases := []struct {
		name   string
		caps   contract.Capabilities
		action contract.Action
	}{
		{"buttons on send", guardCaps(nil), contract.Send{ID: 1, Conversation: "c", Parts: guardText("hi"), Buttons: guardButtons()}},
		{"buttons on edit", guardCaps(func(c *contract.Capabilities) { c.Edit = true }),
			contract.Edit{ID: 1, Conversation: "c", Target: 1, TargetPlatformRefs: []string{"p"}, Parts: guardText("hi"), Buttons: guardButtons()}},
		{"edit", guardCaps(nil), contract.Edit{ID: 1, Conversation: "c", Target: 1, TargetPlatformRefs: []string{"p"}, Parts: guardText("hi")}},
		{"image", guardCaps(nil), contract.Send{ID: 1, Conversation: "c", Parts: guardMedia(contract.OutImage)}},
		{"file", guardCaps(func(c *contract.Capabilities) { c.MediaOut = []contract.MediaKind{contract.MediaImage} }),
			contract.Send{ID: 1, Conversation: "c", Parts: guardMedia(contract.OutFile)}},
		{"audio", guardCaps(func(c *contract.Capabilities) { c.MediaOut = []contract.MediaKind{contract.MediaImage} }),
			contract.Send{ID: 1, Conversation: "c", Parts: guardMedia(contract.OutAudio)}},
		{"media on edit", guardCaps(func(c *contract.Capabilities) { c.Edit = true }),
			contract.Edit{ID: 1, Conversation: "c", Target: 1, TargetPlatformRefs: []string{"p"}, Parts: guardMedia(contract.OutImage)}},
		{"reply_to_platform_ref", guardCaps(nil),
			contract.Send{ID: 1, Conversation: "c", ReplyTo: &one, ReplyToPlatformRef: "p1", Parts: guardText("hi")}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := Guard(tc.caps, tc.action)
			if got == nil {
				t.Fatal("Guard = nil, want an unsupported_capability ack")
			}
			if !reflect.DeepEqual(*got, want) {
				t.Fatalf("ack = %+v, want the unsupported-capability fixture %+v", *got, want)
			}
			if got.Failed == nil || got.Failed.Retryable {
				t.Fatalf("ack = %+v, want a non-retryable failure", got)
			}
		})
	}
}

func TestGuardLetsDeclaredCapabilitiesThrough(t *testing.T) {
	one := uint64(1)
	all := guardCaps(func(c *contract.Capabilities) {
		c.Buttons, c.Edit, c.ReplyTo = true, true, true
		c.MediaOut = []contract.MediaKind{contract.MediaImage, contract.MediaFile, contract.MediaAudio}
	})
	cases := []struct {
		name   string
		caps   contract.Capabilities
		action contract.Action
	}{
		{"buttons", all, contract.Send{ID: 1, Conversation: "c", Parts: guardText("hi"), Buttons: guardButtons()}},
		{"edit", all, contract.Edit{ID: 1, Conversation: "c", Target: 1, TargetPlatformRefs: []string{"p"}, Parts: guardText("hi")}},
		{"image", all, contract.Send{ID: 1, Conversation: "c", Parts: guardMedia(contract.OutImage)}},
		{"file", all, contract.Send{ID: 1, Conversation: "c", Parts: guardMedia(contract.OutFile)}},
		{"audio", all, contract.Send{ID: 1, Conversation: "c", Parts: guardMedia(contract.OutAudio)}},
		{"reply_to_platform_ref", all, contract.Send{ID: 1, Conversation: "c", ReplyTo: &one, ReplyToPlatformRef: "p1", Parts: guardText("hi")}},
		{"plain text needs nothing", guardCaps(nil), contract.Send{ID: 1, Conversation: "c", Parts: guardText("hi")}},
		{"reply_to without a platform ref", guardCaps(nil), contract.Send{ID: 1, Conversation: "c", ReplyTo: &one, Parts: guardText("hi")}},
		{"answer_action", guardCaps(nil), contract.AnswerAction{ID: 1, Conversation: "c", ActionRef: "r", Text: "ok"}},
		{"text over max_text is the adapter's chunking", guardCaps(nil),
			contract.Send{ID: 1, Conversation: "c", Parts: guardText(strings.Repeat("x", 500))}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := Guard(tc.caps, tc.action); got != nil {
				t.Fatalf("Guard = %+v, want nil", *got)
			}
		})
	}
}

func TestGuardRunnerAcksWithoutExecute(t *testing.T) {
	ad := &loopAdapter{} // declares no buttons
	g := newLoopRig(t, ad)
	g.runner.Guard = Guard
	id := g.core.Enqueue(contract.Send{Conversation: "c1", Parts: guardText("hi"), Buttons: guardButtons()})
	stop := g.start()
	g.waitState(id, fakecore.StateFailed)
	stop()
	if got := ad.executions(); len(got) != 0 {
		t.Fatalf("executions = %v, want none", got)
	}
}

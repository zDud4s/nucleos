// §spec pilar-de-browser

package chrome

import (
	"encoding/json"
	"testing"
	"time"

	"nucleosbrowser/cdp/cdptest"
)

// dialogAnswers waits for the driver to answer a dialog and hands back what it said.
//
// Polled, because the event and the answer are on different goroutines: Emit puts the event on the
// wire and the driver handles it on its own dispatch loop, so reading the calls straight afterwards
// is a race that passes on a fast machine and on nothing else.
func dialogAnswers(t *testing.T, fake *cdptest.Browser, want int) []map[string]any {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	for {
		var answers []map[string]any
		for _, call := range fake.Calls() {
			if call.Method != "Page.handleJavaScriptDialog" {
				continue
			}
			var params map[string]any
			if err := json.Unmarshal(call.Params, &params); err != nil {
				t.Fatalf("the answer was unreadable: %v", err)
			}
			answers = append(answers, params)
		}
		if len(answers) >= want {
			return answers
		}
		if !time.Now().Before(deadline) {
			t.Fatalf("the page was answered %d time(s) and asked %d; a question left unanswered freezes"+
				" the renderer for the life of the session", len(answers), want)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// TestAQuestionThePageAsksIsAnsweredAndTheAnswerIsNo.
//
// Two things at once, and the first is the one that matters: the page IS answered. alert, confirm,
// prompt and beforeunload block the renderer until the attached CDP client answers, and this driver
// enables the Page domain everywhere, so the attached client is us. Nothing answered them, and the
// gate measured the result — a click on a confirm() never returned, and the session never worked
// again.
//
// The second is which answer. No, for everything the page can put in front of a person, because the
// question is written on a surface the adversary controls (§6.0) and "Delete everything?" is an
// ordinary confirm: accepting is a decision taken on somebody's behalf, dismissing is declining to
// take one. The fence refuses the POST that would follow either way, but a page that acts on its own
// answer locally is not covered by the fence at all.
//
// beforeunload is the one exception and it goes the other way. It asks "leave this page?" AFTER the
// agent already said goto or back, so answering no would silently cancel the act the agent asked for
// and then report that it worked.
func TestAQuestionThePageAsksIsAnsweredAndTheAnswerIsNo(t *testing.T) {
	for _, one := range []struct {
		kind   string
		accept bool
		answer string
	}{
		{kind: "confirm", accept: false, answer: "dismissed"},
		{kind: "alert", accept: false, answer: "dismissed"},
		{kind: "prompt", accept: false, answer: "dismissed"},
		{kind: "beforeunload", accept: true, answer: "accepted"},
	} {
		t.Run(one.kind, func(t *testing.T) {
			fake, driver := connected(t)
			id := withRef(t, fake, driver)

			fake.Emit("S1", "Page.javascriptDialogOpening", map[string]any{
				"type":    one.kind,
				"message": "Delete everything?",
				"url":     "https://example.org/settings",
			})

			answers := dialogAnswers(t, fake, 1)
			if answers[0]["accept"] != one.accept {
				t.Errorf("a %s was answered accept=%v, want %v", one.kind, answers[0]["accept"], one.accept)
			}

			// And the agent is told, because a question answered in silence is a page doing less than
			// it meant to: the click happened, the confirmation was declined, and a reading that said
			// nothing would leave the agent concluding the button is broken.
			snapshot := snapshotOf(t, driver, id)
			if len(snapshot.Dialogs) != 1 {
				t.Fatalf("the reading carried %d dialogs; the agent has no other way to learn one happened", len(snapshot.Dialogs))
			}
			asked := snapshot.Dialogs[0]
			if asked.Kind != one.kind || asked.Answer != one.answer {
				t.Errorf("the reading said %+v, want kind %q answered %q", asked, one.kind, one.answer)
			}
			if asked.Message != "Delete everything?" {
				t.Errorf("the question itself was lost: %q. What was asked is the half the agent"+
					" needs to decide whether no was the wrong answer", asked.Message)
			}
		})
	}
}

// TestAPageThatAsksForeverDoesNotFillTheReading.
//
// Every question still gets answered — that is not negotiable, because each unanswered one is a
// frozen renderer. What is bounded is how many the READING carries: a page can open dialogs in a
// loop, and an unbounded list would spend the agent's whole turn on one hostile page while telling
// it nothing it did not already know from the first entry.
func TestAPageThatAsksForeverDoesNotFillTheReading(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)

	const asked = dialogsRemembered + 4
	for i := 0; i < asked; i++ {
		fake.Emit("S1", "Page.javascriptDialogOpening", map[string]any{
			"type": "alert", "message": "again", "url": "https://example.org/",
		})
	}

	if answers := dialogAnswers(t, fake, asked); len(answers) != asked {
		t.Errorf("%d of %d questions were answered; the unanswered ones are frozen renderers", len(answers), asked)
	}
	if got := len(snapshotOf(t, driver, id).Dialogs); got != dialogsRemembered {
		t.Errorf("the reading carried %d dialogs, want the cap of %d", got, dialogsRemembered)
	}
}

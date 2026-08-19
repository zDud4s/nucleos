package chrome

import (
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// inWorld tells the driver where a page's world is, which is where the ferry's same-origin rule
// reads the origin from. Chromium reports this; the page never does.
func inWorld(fake *cdptest.Browser, origin string, contextID int64) {
	fake.Emit("S1", "Runtime.executionContextCreated", map[string]any{
		"context": map[string]any{
			"id":      contextID,
			"origin":  origin,
			"auxData": map[string]any{"frameId": "F1"},
		},
	})
}

// asks is the page calling the binding.
func asks(fake *cdptest.Browser, contextID int64, id int64, method, url string) {
	payload, _ := json.Marshal(map[string]any{"id": id, "url": url, "method": method})
	fake.Emit("S1", "Runtime.bindingCalled", map[string]any{
		"name":               ferryBinding,
		"payload":            string(payload),
		"executionContextId": contextID,
	})
}

// answered waits for the reply the ferry evaluates back into the page, and returns it decoded.
//
// Waited for rather than read, because the ferry answers on a goroutine — it has to, since handlers
// run in order on one dispatch loop and carrying a request inline would hold every refusal,
// navigation and lifecycle event behind it for as long as the network takes.
func answered(t *testing.T, fake *cdptest.Browser, within time.Duration) map[string]any {
	t.Helper()
	deadline := time.Now().Add(within)
	for time.Now().Before(deadline) {
		for _, call := range fake.Calls() {
			if call.Method != "Runtime.evaluate" {
				continue
			}
			var params struct {
				Expression string `json:"expression"`
			}
			if err := json.Unmarshal(call.Params, &params); err != nil {
				continue
			}
			open := strings.Index(params.Expression, "{")
			shut := strings.LastIndex(params.Expression, "}")
			if open < 0 || shut <= open {
				continue
			}
			var answer map[string]any
			if err := json.Unmarshal([]byte(params.Expression[open:shut+1]), &answer); err == nil {
				return answer
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("the ferry never answered; calls were %v", fake.Methods())
	return nil
}

func ferrying(t *testing.T) (*cdptest.Browser, *Driver, browser.SessionID) {
	t.Helper()
	fake, driver := connected(t)
	session := opened(t, driver)
	inWorld(fake, "https://example.org", 7)
	return fake, driver, session.ID
}

// TestTheFerryCarriesOnlyGetAndHead.
//
// The method rule, and it is the fence's own: a write is the consequence spec §6.2 exists to refuse,
// and a service that carried one would be a way around the rule rather than a way to read a page.
func TestTheFerryCarriesOnlyGetAndHead(t *testing.T) {
	fake, driver, id := ferrying(t)

	asks(fake, 7, 1, "POST", "https://example.org/orders")

	answer := answered(t, fake, 5*time.Second)
	if answer["ok"] == true {
		t.Fatal("the ferry carried a POST")
	}
	if detail, _ := answer["detail"].(string); !strings.Contains(detail, "POST") {
		t.Errorf("the refusal does not say what was wrong: %q", detail)
	}
	if blocked := snapshotOf(t, driver, id).Blocked; blocked == nil {
		t.Error("the page was refused and the reading did not say so")
	}
}

// TestTheFerryWillNotFetchFromAnotherOrigin.
//
// The rule that makes this a service and not a hole. Same-origin opens no host the page could not
// already reach, and the answer comes from a server the page already IS.
func TestTheFerryWillNotFetchFromAnotherOrigin(t *testing.T) {
	fake, driver, id := ferrying(t)

	asks(fake, 7, 1, "GET", "https://elsewhere.example.net/secrets")

	answer := answered(t, fake, 5*time.Second)
	if answer["ok"] == true {
		t.Fatal("the ferry fetched from another host")
	}
	if detail, _ := answer["detail"].(string); !strings.Contains(detail, "elsewhere.example.net") {
		t.Errorf("the refusal does not name where it would have gone: %q", detail)
	}
	if blocked := snapshotOf(t, driver, id).Blocked; blocked == nil {
		t.Error("the page was refused and the reading did not say so")
	}
}

// TestAnOriginTheDriverWasNeverToldAboutIsNotCarriedFor.
//
// The origin comes from the execution context Chromium reported. A binding call from a world the
// driver has no record of is refused rather than resolved against a guess — a restriction the
// restricted thing gets to describe is not one.
func TestAnOriginTheDriverWasNeverToldAboutIsNotCarriedFor(t *testing.T) {
	fake, _, _ := ferrying(t)

	asks(fake, 99, 1, "GET", "https://example.org/data")

	if answer := answered(t, fake, 5*time.Second); answer["ok"] == true {
		t.Fatal("the ferry carried a request from a world it knows nothing about")
	}
}

// TestAPageThatKeepsAskingIsCutOff.
//
// A page that polls would otherwise have us carrying its traffic for as long as the session lives.
func TestAPageThatKeepsAskingIsCutOff(t *testing.T) {
	fake, _, _ := ferrying(t)

	for i := 0; i <= ferryBudget+1; i++ {
		asks(fake, 7, int64(i+1), "POST", "https://example.org/orders")
	}

	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		if strings.Contains(lastRefusal(fake), fmt.Sprintf("%d requests", ferryBudget)) {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("a page asked past the budget and was never cut off; last was %q", lastRefusal(fake))
}

func lastRefusal(fake *cdptest.Browser) string {
	last := ""
	for _, call := range fake.Calls() {
		if call.Method != "Runtime.evaluate" {
			continue
		}
		var params struct {
			Expression string `json:"expression"`
		}
		if err := json.Unmarshal(call.Params, &params); err == nil {
			last = params.Expression
		}
	}
	return last
}

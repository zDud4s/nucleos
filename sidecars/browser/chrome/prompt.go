// §spec browser-volante

package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// This file is how a page's question reaches a person who is not in this process.
//
// A prompt is raised from the connection's one dispatch goroutine, which the fence also answers on, so
// raising one never calls out: it files the prompt, starts its timer and tells the viewers. The page
// stays frozen on its dialog until somebody answers it, and that somebody is Answer (the person), the
// timer (nobody came) or EndPerson (the turn is over). Whichever gets there first takes the prompt out
// of the pending set; the others find it gone. That is what makes the answer happen exactly once.

// promptTimeoutDefault is how long a prompt waits for its person before it is cancelled.
const promptTimeoutDefault = 120 * time.Second

// promptsKept bounds the pending prompts of one turn, so a page that opens dialogs in a loop cannot
// grow the set without limit.
const promptsKept = 32

// pendingPrompt is one question waiting for an answer.
type pendingPrompt struct {
	prompt browser.Prompt
	on     cdp.SessionID
	// cancel declines the question on the page, as a person who walked away would.
	cancel func(ctx context.Context)
	timer  *time.Timer
	// requestID is the paused request an auth prompt answers; empty for every other kind.
	requestID string
	// target is what a select or file prompt needs to answer its page; nil for every other kind.
	target any
}

// raisePrompt files a prompt for this turn, starts its timer and streams it to every viewer. It makes
// no CDP call. It reports whether the prompt was filed, which it is not when the set is full.
func (d *Driver) raisePrompt(state *personState, on cdp.SessionID, prompt browser.Prompt, cancel func(ctx context.Context)) bool {
	return d.raisePromptFor(state, on, prompt, cancel, "")
}

// raisePromptFor is raisePrompt for a prompt that answers one paused request, named by requestID.
func (d *Driver) raisePromptFor(state *personState, on cdp.SessionID, prompt browser.Prompt, cancel func(ctx context.Context), requestID string, target ...any) bool {
	state.pmu.Lock()
	defer state.pmu.Unlock()
	if len(state.pending) >= promptsKept {
		return false
	}
	state.seq++
	prompt.ID = fmt.Sprintf("p%d", state.seq)
	pending := &pendingPrompt{prompt: prompt, on: on, cancel: cancel, requestID: requestID}
	if len(target) > 0 {
		pending.target = target[0]
	}
	if state.pending == nil {
		state.pending = map[string]*pendingPrompt{}
	}
	state.pending[prompt.ID] = pending
	state.order = append(state.order, prompt.ID)
	id := prompt.ID
	pending.timer = time.AfterFunc(d.promptTimeout, func() {
		if gone := d.takePrompt(state, id); gone != nil {
			d.cancelPrompt(gone)
		}
	})
	d.emitPrompt(on, prompt)
	return true
}

// takePrompt removes a prompt from the pending set, once. The caller that gets it back is the one that
// answers the page; the viewers are told it is over.
func (d *Driver) takePrompt(state *personState, id string) *pendingPrompt {
	state.pmu.Lock()
	defer state.pmu.Unlock()
	pending, ok := state.pending[id]
	if !ok {
		return nil
	}
	delete(state.pending, id)
	for i, one := range state.order {
		if one == id {
			state.order = append(state.order[:i:i], state.order[i+1:]...)
			break
		}
	}
	if pending.timer != nil {
		pending.timer.Stop()
	}
	d.emitPrompt(pending.on, browser.Prompt{ID: id, Kind: pending.prompt.Kind, Resolved: true})
	return pending
}

// cancelPrompt declines a taken prompt on its page, under its own bounded context.
func (d *Driver) cancelPrompt(pending *pendingPrompt) {
	if pending.cancel == nil {
		return
	}
	ctx, stop := context.WithTimeout(context.Background(), dialogAnswerWithin)
	defer stop()
	pending.cancel(ctx)
}

// cancelAllPrompts declines every pending prompt of the turn. EndPerson calls it before the fence
// returns, so no question outlives the person who could have answered it.
func (d *Driver) cancelAllPrompts(state *personState) {
	state.pmu.Lock()
	ids := append([]string(nil), state.order...)
	state.pmu.Unlock()
	for _, id := range ids {
		if pending := d.takePrompt(state, id); pending != nil {
			d.cancelPrompt(pending)
		}
	}
}

// emitPrompt hands a prompt frame to every viewer of the page. Never blocks: a viewer whose queue is
// full misses it.
func (d *Driver) emitPrompt(on cdp.SessionID, prompt browser.Prompt) {
	d.mu.Lock()
	defer d.mu.Unlock()
	cast := d.casts[on]
	if cast == nil {
		return
	}
	for v := range cast.viewers {
		v.sendPrompt(prompt)
	}
}

// Answer delivers a person's answer to a pending prompt of their session.
//
// The person's turn is checked first, then the prompt. The answer is validated before the prompt is
// taken, so a malformed one leaves the question open for a better one.
func (d *Driver) Answer(ctx context.Context, id browser.SessionID, promptID string, answer json.RawMessage) error {
	d.gate.RLock()
	defer d.gate.RUnlock()
	state := d.person.Load()
	if state == nil || state.session != id {
		return browser.ErrNotPerson
	}
	state.pmu.Lock()
	pending, ok := state.pending[promptID]
	state.pmu.Unlock()
	if !ok {
		return browser.ErrNoPrompt
	}

	switch pending.prompt.Kind {
	case "dialog":
		var reply struct {
			Accept bool   `json:"accept"`
			Text   string `json:"text"`
		}
		if err := json.Unmarshal(answer, &reply); err != nil {
			return browser.ErrBadAnswer
		}
		if d.takePrompt(state, promptID) == nil {
			return browser.ErrNoPrompt
		}
		_, err := d.conn.Call(ctx, pending.on, "Page.handleJavaScriptDialog", map[string]any{
			"accept": reply.Accept, "promptText": reply.Text,
		})
		return err
	case "auth":
		return d.answerAuthPrompt(ctx, state, pending, promptID, answer)
	case "select":
		return d.answerSelect(ctx, state, pending, promptID, answer)
	case "file":
		return d.answerFile(ctx, state, id, pending, promptID, answer)
	}
	return browser.ErrBadAnswer
}

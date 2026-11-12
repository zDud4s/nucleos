// §spec browser-volante

package chrome

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"strings"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// A file input opens a native dialog that a screencast never shows. While a person drives, the page's
// chooser is intercepted (person.go) and becomes a `file` prompt; the person's answer is the files, and
// what is allowed onto the disk is bounded here.

const (
	// fileMost bounds one file a person sends. The agent's own upload keeps its smaller attachMost.
	fileMost = 10 << 20
	// filesMost bounds how many files one answer carries.
	filesMost = 16
)

// fileTarget is what a file prompt needs to answer the page: the input the chooser was opened for.
type fileTarget struct {
	backendNodeID int
}

// acceptQuestion reads the accept attribute of the input a chooser was opened for.
const acceptQuestion = `function() { return String(this.accept || ''); }`

// onFileChooser turns a chooser the page opened into a prompt, while a person drives.
func (d *Driver) onFileChooser(event cdp.Event) {
	if event.Method != "Page.fileChooserOpened" {
		return
	}
	state := d.person.Load()
	if state == nil || state.page != event.Session {
		return
	}
	var opened struct {
		Mode          string `json:"mode"`
		BackendNodeID int    `json:"backendNodeId"`
	}
	if err := json.Unmarshal(event.Params, &opened); err != nil {
		return
	}
	// Never on the dispatch goroutine: reading the accept attribute is a CDP call, and this goroutine
	// is the one that answers those calls.
	go d.raiseFilePrompt(state, event.Session, opened.Mode, opened.BackendNodeID)
}

// raiseFilePrompt asks the page for the input's accept and files the prompt. The gate is held shared,
// so a turn that ends meanwhile is waited for and the prompt is then dropped, not raised into the void.
func (d *Driver) raiseFilePrompt(state *personState, on cdp.SessionID, mode string, node int) {
	d.gate.RLock()
	defer d.gate.RUnlock()
	if d.person.Load() != state {
		return
	}
	ctx, stop := context.WithTimeout(context.Background(), dialogAnswerWithin)
	defer stop()

	accept := ""
	if resolved, err := d.conn.Call(ctx, on, "DOM.resolveNode", map[string]any{"backendNodeId": node}); err == nil {
		var object struct {
			Object struct {
				ObjectID string `json:"objectId"`
			} `json:"object"`
		}
		if json.Unmarshal(resolved, &object) == nil && object.Object.ObjectID != "" {
			if value, err := d.callOnValue(ctx, on, object.Object.ObjectID, acceptQuestion, ""); err == nil {
				accept = value
			}
		}
	}
	multiple := mode == "selectMultiple"
	d.raisePromptFor(state, on, browser.Prompt{Kind: "file", Multiple: &multiple, Accept: accept}, nil, "", fileTarget{backendNodeID: node})
}

// answerFile applies a person's answer to a file prompt: nothing for a cancel, otherwise every file
// validated first, then written, then named to the page.
func (d *Driver) answerFile(ctx context.Context, state *personState, id browser.SessionID, pending *pendingPrompt, promptID string, answer json.RawMessage) error {
	var reply struct {
		Cancel bool `json:"cancel"`
		Files  []struct {
			Name string `json:"name"`
			Mime string `json:"mime"`
			Data string `json:"data_b64"`
		} `json:"files"`
	}
	if err := json.Unmarshal(answer, &reply); err != nil {
		return browser.ErrBadAnswer
	}
	if reply.Cancel {
		if d.takePrompt(state, promptID) == nil {
			return browser.ErrNoPrompt
		}
		return nil
	}
	target, ok := pending.target.(fileTarget)
	if !ok || len(reply.Files) == 0 || len(reply.Files) > filesMost {
		return browser.ErrBadAnswer
	}
	if pending.prompt.Multiple != nil && !*pending.prompt.Multiple && len(reply.Files) > 1 {
		return browser.ErrBadAnswer
	}

	type decoded struct {
		name string
		data []byte
	}
	var files []decoded
	seen := map[string]bool{}
	for _, one := range reply.Files {
		name := strings.TrimSpace(one.Name)
		if checkAttachment(browser.Action{Filename: name}) != nil || seen[name] {
			return browser.ErrBadAnswer
		}
		seen[name] = true
		if len(one.Data) > base64.StdEncoding.EncodedLen(fileMost) {
			return browser.ErrBadAnswer
		}
		data, err := base64.StdEncoding.DecodeString(one.Data)
		if err != nil || len(data) > fileMost {
			return browser.ErrBadAnswer
		}
		files = append(files, decoded{name: name, data: data})
	}

	if d.takePrompt(state, promptID) == nil {
		return browser.ErrNoPrompt
	}
	d.mu.Lock()
	entry := d.sessions[id]
	d.mu.Unlock()
	if entry == nil {
		return browser.ErrNoSuchSession
	}
	paths := make([]string, 0, len(files))
	for _, file := range files {
		// A Go string carries arbitrary bytes, so the conversion is lossless: what lands on disk is
		// exactly what was decoded.
		path, err := d.writeAttachment(entry, browser.Action{Filename: file.name, Text: string(file.data)})
		if err != nil {
			return err
		}
		paths = append(paths, path)
	}
	_, err := d.conn.Call(ctx, pending.on, "DOM.setFileInputFiles", map[string]any{
		"files": paths, "backendNodeId": target.backendNodeID,
	})
	return err
}

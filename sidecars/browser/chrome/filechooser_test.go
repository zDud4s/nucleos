// §spec browser-volante

package chrome

import (
	"bytes"
	"context"
	"encoding/base64"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// The file-chooser group. A file input opens a native dialog nobody can see in a screencast, so while
// the person drives the chooser is intercepted, becomes a prompt, and the person's answer is the files.
// What matters here is what is allowed onto the disk: a bound on size, a name that is only a name, and
// nothing written at all unless every file passes.

const tenMiB = 10 << 20

// chooserPage makes the fake page answer what a file chooser needs: the input resolves to an object
// and reports its accept attribute.
func chooserPage(fake *cdptest.Browser, accept string) {
	fake.Handle("DOM.resolveNode", func(cdptest.Call) (any, error) {
		return map[string]any{"object": map[string]any{"objectId": "FILEOBJ"}}, nil
	})
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		return map[string]any{"result": map[string]any{"type": "string", "value": accept}}, nil
	})
}

// openChooser has the page open a chooser and waits for the prompt that asks about it.
func openChooser(t *testing.T, fake *cdptest.Browser, driver *Driver, id browser.SessionID, log *promptLog, mode string) browser.Prompt {
	t.Helper()
	fake.Emit(string(cdpOf(driver, id)), "Page.fileChooserOpened", map[string]any{
		"frameId": "F1", "mode": mode, "backendNodeId": 9,
	})
	return log.openPrompt(t, "file")
}

func fileEntry(name string, data []byte) map[string]any {
	return map[string]any{"name": name, "mime": "application/octet-stream", "data_b64": base64.StdEncoding.EncodeToString(data)}
}

// attachDirOf is the session's attachment directory, or "" when nothing was ever written.
func attachDirOf(driver *Driver, id browser.SessionID) string {
	driver.mu.Lock()
	defer driver.mu.Unlock()
	return driver.sessions[id].attachDir
}

// nothingWritten fails when the session's attachment directory holds anything.
func nothingWritten(t *testing.T, driver *Driver, id browser.SessionID) {
	t.Helper()
	dir := attachDirOf(driver, id)
	if dir == "" {
		return
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("reading the attachment directory: %v", err)
	}
	if len(entries) != 0 {
		t.Errorf("a refused answer left %d file(s) on disk", len(entries))
	}
}

// TestBeginPersonInterceptsTheFileChooserAndEndPersonStops. Interception is on for exactly the
// person's turn, on their page, and failing to turn it on is failing to begin: a person who could not
// choose files would be handed a browser that silently ignores their upload.
func TestBeginPersonInterceptsTheFileChooserAndEndPersonStops(t *testing.T) {
	fake, driver, id := personSession(t)
	on := string(cdpOf(driver, id))
	beginPerson(t, driver, id)

	calls := callsTo(fake, "Page.setInterceptFileChooserDialog")
	if len(calls) != 1 || calls[0].Session != on || paramsMap(t, calls[0])["enabled"] != true {
		t.Fatalf("after BeginPerson the interception calls = %+v, want one enabled=true on the page", calls)
	}
	endPerson(t, driver, id)
	calls = callsTo(fake, "Page.setInterceptFileChooserDialog")
	if len(calls) != 2 || calls[1].Session != on || paramsMap(t, calls[1])["enabled"] != false {
		t.Errorf("after EndPerson the interception calls = %+v, want a second one, enabled=false", calls)
	}

	failing, failingDriver, failingID := personSession(t)
	failing.Handle("Page.setInterceptFileChooserDialog", func(cdptest.Call) (any, error) {
		return nil, errors.New("not supported")
	})
	if err := failingDriver.BeginPerson(context.Background(), failingID); err == nil {
		t.Error("BeginPerson succeeded although the file chooser could not be intercepted")
	}
}

// TestAFileChooserBecomesAPromptAndTheAnswerSetsTheFiles. The chooser is raised as a file prompt that
// carries the input's accept; the answer's bytes land on disk exactly as sent and are named to the page
// by path.
func TestAFileChooserBecomesAPromptAndTheAnswerSetsTheFiles(t *testing.T) {
	fake, driver, id, log := personWatched(t)
	chooserPage(fake, ".png,.txt")
	on := string(cdpOf(driver, id))

	prompt := openChooser(t, fake, driver, id, log, "selectMultiple")
	if prompt.Multiple == nil || !*prompt.Multiple || prompt.Accept != ".png,.txt" {
		t.Errorf("the prompt = %+v, want multiple=true and the input's accept", prompt)
	}

	data := []byte{0xff, 0x00, 0xfe, 'h', 'i', 0x00}
	err := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{
		"files": []any{fileEntry("a.bin", data)},
	}))
	if err != nil {
		t.Fatalf("Answer: %v", err)
	}

	set := waitForCall(t, fake, "DOM.setFileInputFiles")
	if set.Session != on {
		t.Errorf("files set on session %q, want the page that asked, %q", set.Session, on)
	}
	params := paramsMap(t, set)
	paths, _ := params["files"].([]any)
	if len(paths) != 1 {
		t.Fatalf("setFileInputFiles named %v, want one path", params["files"])
	}
	path, _ := paths[0].(string)
	if filepath.Base(path) != "a.bin" {
		t.Errorf("the file is named %q, want it kept as a.bin", filepath.Base(path))
	}
	if params["backendNodeId"] != float64(9) {
		t.Errorf("setFileInputFiles went to node %v, want the chooser's node 9", params["backendNodeId"])
	}
	onDisk, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading what was written: %v", err)
	}
	if !bytes.Equal(onDisk, data) {
		t.Errorf("the file on disk is %v, want the decoded bytes %v", onDisk, data)
	}
	log.resolved(t, prompt.ID)
}

// TestAFileOverTenMiBIsRefused. The bound is per file and the refusal comes before anything is written;
// the prompt stays open, so the person can pick a smaller file.
func TestAFileOverTenMiBIsRefused(t *testing.T) {
	fake, driver, id, log := personWatched(t)
	chooserPage(fake, "")
	prompt := openChooser(t, fake, driver, id, log, "selectSingle")

	err := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{
		"files": []any{fileEntry("small.bin", []byte("ok")), fileEntry("big.bin", make([]byte, tenMiB+1))},
	}))
	if !errors.Is(err, browser.ErrBadAnswer) {
		t.Fatalf("Answer = %v, want ErrBadAnswer", err)
	}
	if hasCall(fake, "DOM.setFileInputFiles") {
		t.Error("files were set although one of them was over the bound")
	}
	nothingWritten(t, driver, id)

	if err := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{"cancel": true})); err != nil {
		t.Errorf("the prompt was closed by the refused answer: %v", err)
	}
}

// TestAFileNameOver128IsRefused. A name is at most 128 characters and only ever a name; each bad name is
// refused with the good file beside it left unwritten.
func TestAFileNameOver128IsRefused(t *testing.T) {
	for _, one := range []struct{ why, name string }{
		{"one past the limit", strings.Repeat("a", 129)},
		{"a relative path", "dir/report.txt"},
		{"a windows path", `dir\report.txt`},
		{"a climb", "..report.txt"},
	} {
		t.Run(one.why, func(t *testing.T) {
			fake, driver, id, log := personWatched(t)
			chooserPage(fake, "")
			prompt := openChooser(t, fake, driver, id, log, "selectSingle")

			err := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{
				"files": []any{fileEntry("good.txt", []byte("fine")), fileEntry(one.name, []byte("x"))},
			}))
			if !errors.Is(err, browser.ErrBadAnswer) {
				t.Fatalf("Answer = %v, want ErrBadAnswer", err)
			}
			if hasCall(fake, "DOM.setFileInputFiles") {
				t.Error("files were set although one of them had a bad name")
			}
			nothingWritten(t, driver, id)
		})
	}

	t.Run("exactly 128 is allowed", func(t *testing.T) {
		fake, driver, id, log := personWatched(t)
		chooserPage(fake, "")
		prompt := openChooser(t, fake, driver, id, log, "selectSingle")
		name := strings.Repeat("a", 124) + ".txt"
		err := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{
			"files": []any{fileEntry(name, []byte("fine"))},
		}))
		if err != nil {
			t.Errorf("Answer = %v, want a 128-character name accepted", err)
		}
	})
}

// TestACancelledFilePromptSetsNothing. Cancelling resolves the prompt and touches neither the page's
// input nor the disk.
func TestACancelledFilePromptSetsNothing(t *testing.T) {
	fake, driver, id, log := personWatched(t)
	chooserPage(fake, "")
	prompt := openChooser(t, fake, driver, id, log, "selectSingle")

	err := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{"cancel": true}))
	if err != nil {
		t.Fatalf("Answer: %v", err)
	}
	log.resolved(t, prompt.ID)
	if hasCall(fake, "DOM.setFileInputFiles") {
		t.Error("a cancelled file prompt still set files")
	}
	nothingWritten(t, driver, id)
	again := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{"cancel": true}))
	if !errors.Is(again, browser.ErrNoPrompt) {
		t.Errorf("a second answer = %v, want ErrNoPrompt", again)
	}
}

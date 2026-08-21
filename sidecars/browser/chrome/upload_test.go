package chrome

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// The upload verb, at the level where a filename becomes a real file.
//
// Everything here is about the ONE thing this verb could get wrong that the others cannot: it writes
// to a disk. The contents are the agent's own words and carry no new risk — anything it can put in
// them it could have typed into a form field — but the NAME becomes a path, and a name that is
// allowed to be a path is the confused deputy with the filesystem in place of the site list.

// answersFileInput makes the fake page say what the element is.
func answersFileInput(fake *cdptest.Browser, answer string) {
	fake.Handle("Runtime.callFunctionOn", func(call cdptest.Call) (any, error) {
		if strings.Contains(string(call.Params), "not-a-file-input") {
			return map[string]any{"result": map[string]any{"value": answer}}, nil
		}
		return reachable(), nil
	})
	fake.Handle("DOM.resolveNode", func(cdptest.Call) (any, error) {
		return map[string]any{"object": map[string]any{"objectId": "OBJ"}}, nil
	})
}

func uploading(name, contents string) browser.Action {
	return browser.Action{Kind: browser.ActionUpload, Ref: "e1", Filename: name, Text: contents}
}

// TestAFilenameIsANameAndNeverAPath.
//
// Each of these is its own refusal on purpose. "That is not a valid filename" tells an agent nothing
// it can act on; "a filename has no directories in it" tells it what to send instead. And each is
// checked BEFORE anything is written, so a refused upload leaves nothing on the disk to explain.
func TestAFilenameIsANameAndNeverAPath(t *testing.T) {
	for _, one := range []struct {
		why  string
		name string
		says string
	}{
		{"a relative path", "notes/report.txt", "directory"},
		{"a windows path", `notes\report.txt`, "directory"},
		{"an absolute path", "/etc/passwd", "directory"},
		{"a drive letter", "C:report.txt", "colon"},
		{"a climb", "..report.txt", ".."},
		{"a climbing path", "../../secrets.txt", "directory"},
		{"nothing at all", "   ", "needs a filename"},
		{"a hidden name", ".bashrc", "starts with a dot"},
		{"a control character", "report\x00.txt", "control character"},
		{"a name longer than any filesystem takes", strings.Repeat("a", 400), "characters"},
	} {
		t.Run(one.why, func(t *testing.T) {
			refusal := checkAttachment(uploading(one.name, "hello"))
			if refusal == nil {
				t.Fatalf("%q was accepted as a filename", one.name)
			}
			if refusal.Consequence != browser.ConsequenceNotApplicable {
				t.Fatalf("consequence = %q; this is the verb not applying, not the fence refusing",
					refusal.Consequence)
			}
			if !strings.Contains(refusal.Detail, one.says) {
				t.Fatalf("detail = %q, want it to mention %q so the agent knows what to send "+
					"instead", refusal.Detail, one.says)
			}
		})
	}

	if refusal := checkAttachment(uploading("report.txt", "hello")); refusal != nil {
		t.Fatalf("an ordinary filename was refused: %+v", refusal)
	}
}

// TestAFileBiggerThanTheBoundIsRefusedBeforeItIsWritten.
func TestAFileBiggerThanTheBoundIsRefusedBeforeItIsWritten(t *testing.T) {
	refusal := checkAttachment(uploading("big.txt", strings.Repeat("x", attachMost+1)))

	if refusal == nil {
		t.Fatal("an unbounded upload was accepted; a page can talk an agent into filling a disk")
	}
	if !strings.Contains(refusal.Detail, "bytes") {
		t.Fatalf("detail = %q, want it to say how big is too big", refusal.Detail)
	}
}

// TestUploadingToSomethingThatIsNotAFileInputSaysWhatItIs.
//
// The same shape `choose` uses for a dropdown that is not a <select>, and for the same reason: the
// accessibility tree says what a control is FOR and DOM.setFileInputFiles needs what it IS. Without
// this the failure arrives as a CDP error, which reads to an agent like a broken browser rather than
// like a ref pointing at the wrong thing.
func TestUploadingToSomethingThatIsNotAFileInputSaysWhatItIs(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)
	answersFileInput(fake, "not-a-file-input:input type=text")
	driver.mu.Lock()
	entry := driver.sessions[session.ID]
	entry.refs["e1"] = nodeKey{session: entry.cdp, backend: 42}
	driver.mu.Unlock()

	result, err := driver.Act(context.Background(), session.ID, uploading("report.txt", "hello"))
	if err != nil {
		t.Fatalf("act: %v", err)
	}

	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("outcome = %q, want refused", result.Outcome)
	}
	if !strings.Contains(result.Refusal.Detail, "input type=text") {
		t.Fatalf("detail = %q, want it to name what the element actually is",
			result.Refusal.Detail)
	}
	for _, call := range fake.Calls() {
		if call.Method == "DOM.setFileInputFiles" {
			t.Fatal("a file was set on an element that is not a file input")
		}
	}
}

// TestTheAttachmentGoesAwayWithTheSession.
//
// The file has to outlive the ACT — Chromium reads it when the form is submitted, which is a later
// click — and must not outlive the SESSION. Both halves are asserted here because they pull in
// opposite directions, and getting either wrong is silent: too early and the form submits nothing
// while reporting success, too late and the agent's own words stay on a disk nobody asked it to
// write to.
func TestTheAttachmentGoesAwayWithTheSession(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)
	answersFileInput(fake, "ok")
	driver.mu.Lock()
	entry := driver.sessions[session.ID]
	entry.refs["e1"] = nodeKey{session: entry.cdp, backend: 42}
	driver.mu.Unlock()

	result, err := driver.Act(context.Background(), session.ID, uploading("report.txt", "the body"))
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("upload was refused: %+v", result.Refusal)
	}

	driver.mu.Lock()
	dir := entry.attachDir
	driver.mu.Unlock()
	if dir == "" {
		t.Fatal("nothing was written, so there was nothing for the browser to attach")
	}
	written, err := os.ReadFile(filepath.Join(dir, "report.txt"))
	if err != nil {
		t.Fatalf("the file is not under the name the site is told: %v", err)
	}
	if string(written) != "the body" {
		t.Fatalf("the file holds %q", written)
	}

	// And it survives the act that wrote it, because the submission is a later one.
	if _, err := os.Stat(dir); err != nil {
		t.Fatalf("the directory went away when the upload returned, so a form submitted "+
			"afterwards would carry nothing: %v", err)
	}

	if err := driver.Close(context.Background(), session.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Fatalf("the agent's file outlived the session it belonged to: %v", err)
	}
}

// TestUploadDoesNotOpenTheWriteWindow.
//
// Attaching is not sending, and the distinction is the whole reason this verb needed no new fence
// rule. The window is a permission for ONE submission, opened by the act that causes it; opening it
// on an upload would spend it on an act that sends nothing and shut it again before the click that
// does.
func TestUploadDoesNotOpenTheWriteWindow(t *testing.T) {
	if opensAForm(browser.ActionUpload) {
		t.Fatal("upload arms the write window; it sends nothing, so the permission it opens " +
			"belongs to no submission and is gone before the one that follows")
	}
	if !needsRef(browser.ActionUpload) {
		t.Fatal("upload does not require a ref, so it could be aimed at something no snapshot showed")
	}
}

// TestABadFilenameIsRefusedThroughTheVERB.
//
// TestAFilenameIsANameAndNeverAPath above asks `checkAttachment` directly, which proves the rule and
// NOT that anything calls it. That gap is not hypothetical: removing the call from `attach` left
// every case up there passing, and a traversal would have reached the disk with a full green suite.
//
// So this one goes through Act, and asserts the two things the pure test cannot — that the check runs
// at all, and that it runs BEFORE anything is written.
func TestABadFilenameIsRefusedThroughTheVERB(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)
	answersFileInput(fake, "ok")
	driver.mu.Lock()
	entry := driver.sessions[session.ID]
	entry.refs["e1"] = nodeKey{session: entry.cdp, backend: 42}
	driver.mu.Unlock()

	result, err := driver.Act(context.Background(), session.ID,
		uploading("../../escaped.txt", "anything"))
	if err != nil {
		t.Fatalf("act: %v", err)
	}

	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("a filename with a path in it was accepted by the verb; the rule exists and " +
			"nothing calls it")
	}
	driver.mu.Lock()
	dir := entry.attachDir
	driver.mu.Unlock()
	if dir != "" {
		t.Fatal("a refused upload made room for itself on the disk, so the check runs after the " +
			"write rather than before it")
	}
	for _, call := range fake.Calls() {
		if call.Method == "DOM.setFileInputFiles" {
			t.Fatal("a refused upload was still handed to the browser")
		}
	}
}

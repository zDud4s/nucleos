package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unicode"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// attachMost bounds what one upload may carry.
//
// A megabyte, and the number matters less than the fact that there is one: the contents arrive
// through a tool call, so they have already been paid for once in an agent's context, and anything
// approaching this size got there by a loop rather than by intent. It is also what stops a page from
// talking an agent into filling a disk.
const attachMost = 1 << 20

// attachNameMost is how long a filename may be. Well under every filesystem's limit, and long enough
// for anything a person would recognise.
const attachNameMost = 128

// fileInputQuestion asks the page what it is being asked to attach to.
//
// Asked before anything is written, and asked of the PAGE rather than assumed from the ref's role,
// for the reason `choose` asks the same kind of question about a dropdown: the accessibility tree
// says what a control is FOR, and `DOM.setFileInputFiles` needs what it IS. A textbox styled to look
// like a drop zone reads as a textbox; setting files on it fails in a way that surfaces as a CDP
// error rather than as something an agent can act on.
const fileInputQuestion = `function() {
  if (this.tagName !== 'INPUT') { return 'not-a-file-input:' + this.tagName.toLowerCase(); }
  const kind = (this.type || '').toLowerCase();
  if (kind !== 'file') { return 'not-a-file-input:input type=' + kind; }
  if (this.disabled) { return 'disabled'; }
  return 'ok';
}`

// attach puts a file the agent wrote onto a file input.
//
// The order is the point: ask the page what the element is, validate the name, write the bytes, then
// tell Chromium. Every refusal that is going to happen happens before anything reaches a disk, so a
// refused upload leaves nothing behind to clean up or to explain.
func (d *Driver) attach(ctx context.Context, entry *session, on cdp.SessionID, objectID string, action browser.Action) (*browser.Refusal, error) {
	outcome, err := d.callOnValue(ctx, on, objectID, fileInputQuestion, "")
	if err != nil {
		return nil, err
	}
	switch {
	case outcome == "ok":
	case outcome == "disabled":
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			Detail:      "that file input is disabled; something on the page has to enable it first",
		}, nil
	case strings.HasPrefix(outcome, "not-a-file-input:"):
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			// Named as a tag rather than with an article, because the tag can BE "a" — a link
			// refused this way read "that is a a, not a file input", which is what a real session
			// produced the first time this refusal fired against a live page. Angle brackets also
			// pair it with the `<input type=file>` at the end, so the two halves are the same kind
			// of thing.
			Detail: fmt.Sprintf("that ref names <%s>, not a file input; upload attaches to <input type=file>",
				strings.TrimPrefix(outcome, "not-a-file-input:")),
		}, nil
	default:
		return nil, fmt.Errorf("chrome: the page answered %q to an upload", outcome)
	}

	if refusal := checkAttachment(action); refusal != nil {
		return refusal, nil
	}

	path, err := d.writeAttachment(entry, action)
	if err != nil {
		return nil, err
	}

	_, err = d.conn.Call(ctx, on, "DOM.setFileInputFiles", map[string]any{
		"objectId": objectID,
		"files":    []string{path},
	})
	if err != nil {
		return nil, fmt.Errorf("attaching %s: %w", action.Filename, err)
	}
	return nil, nil
}

// checkAttachment refuses everything about an upload that can be judged without touching a disk.
//
// The name is checked as a NAME and never resolved as a path, and the two are not the same check.
// Resolving would mean deciding what `../x` refers to; refusing means never having a question. Every
// clause below is a separate refusal on purpose: "that is not a valid filename" tells an agent
// nothing it can act on, while "a filename has no directories in it" tells it what to send instead.
func checkAttachment(action browser.Action) *browser.Refusal {
	name := strings.TrimSpace(action.Filename)
	switch {
	case name == "":
		return notApplicable("upload needs a filename to send the file under")
	case len(name) > attachNameMost:
		return notApplicable(fmt.Sprintf("that filename is %d characters and the most is %d",
			len(name), attachNameMost))
	case strings.ContainsAny(name, `/\`):
		return notApplicable(fmt.Sprintf(
			"%q has a directory in it; upload takes a NAME and chooses where the file goes itself", name))
	case strings.Contains(name, ".."):
		return notApplicable(fmt.Sprintf("%q has .. in it, and a filename does not", name))
	case strings.Contains(name, ":"):
		// Windows reads `name:stream` as an alternate data stream and `C:` as a drive. Neither is a
		// filename anywhere, so this refuses on every platform rather than only where it would bite.
		return notApplicable(fmt.Sprintf("%q has a colon in it, and a filename does not", name))
	case name == "." || strings.HasPrefix(name, "."):
		// A leading dot is not dangerous; it is a name nobody meant. Refusing it keeps the reported
		// filename something a person reading the record can recognise.
		return notApplicable(fmt.Sprintf("%q starts with a dot; give the file a name a person would read", name))
	case strings.IndexFunc(name, isUnprintable) >= 0:
		return notApplicable("that filename has a control character in it")
	case len(action.Text) > attachMost:
		return notApplicable(fmt.Sprintf("that file is %d bytes and the most one upload carries is %d",
			len(action.Text), attachMost))
	}
	return nil
}

func isUnprintable(r rune) bool {
	return r < 0x20 || r == 0x7f || !unicode.IsPrint(r)
}

func notApplicable(detail string) *browser.Refusal {
	return &browser.Refusal{Consequence: browser.ConsequenceNotApplicable, Detail: detail}
}

// writeAttachment puts the contents somewhere Chromium can read them.
//
// One directory per SESSION rather than one per upload, and it outlives the act on purpose: Chromium
// reads a file input's file when the form is SUBMITTED, which is a later act than the one that
// attached it. A file deleted when the upload returned would be a form that submits an empty
// attachment, and — worse — one that says it submitted successfully. `Close` is where it goes.
func (d *Driver) writeAttachment(entry *session, action browser.Action) (string, error) {
	d.mu.Lock()
	dir := entry.attachDir
	d.mu.Unlock()

	if dir == "" {
		made, err := os.MkdirTemp("", "nucleos-attach-")
		if err != nil {
			return "", fmt.Errorf("making room for the attachment: %w", err)
		}
		d.mu.Lock()
		// Checked again under the lock: two acts on one session do not run at once today, and a
		// directory leaked because that changed is a directory nobody ever finds.
		if entry.attachDir == "" {
			entry.attachDir = made
		}
		dir = entry.attachDir
		d.mu.Unlock()
		if dir != made {
			_ = os.RemoveAll(made)
		}
	}

	path := filepath.Join(dir, strings.TrimSpace(action.Filename))
	if err := os.WriteFile(path, []byte(action.Text), 0o600); err != nil {
		return "", fmt.Errorf("writing the attachment: %w", err)
	}
	return path, nil
}

// forgetAttachments removes what a session wrote. Errors are swallowed: this runs while a session is
// going away, and a temp directory that will not delete is not a reason to fail closing a browser.
func (d *Driver) forgetAttachments(entry *session) {
	d.mu.Lock()
	dir := entry.attachDir
	entry.attachDir = ""
	d.mu.Unlock()
	if dir != "" {
		_ = os.RemoveAll(dir)
	}
}

// attachedTo reports what files are currently sitting on the forms of one document, by NAME.
//
// Read at the moment a form is armed, so the record of what left can say a file went with it. Names
// only — the contents are the one thing that must never be written down, for the reason migration
// 0097 gives about field values, and more so: a file is the most concentrated form there is of
// content that should not come to rest in this database.
const attachedQuestion = `function() {
  const form = this.form
    || (this.tagName === 'FORM' ? this : (this.closest ? this.closest('form') : null));
  if (!form) { return '[]'; }
  const names = [];
  for (const one of Array.from(form.elements || [])) {
    if (!one || (one.type || '').toLowerCase() !== 'file') { continue; }
    for (const file of Array.from(one.files || [])) {
      names.push(String(file.name).slice(0, 128));
    }
  }
  return JSON.stringify(names.slice(0, 16));
}`

// attachedNames asks one element's form what it is carrying.
func (d *Driver) attachedNames(ctx context.Context, on cdp.SessionID, objectID string) []string {
	answer, err := d.callOnValue(ctx, on, objectID, attachedQuestion, "")
	if err != nil || answer == "" {
		return nil
	}
	var names []string
	if err := json.Unmarshal([]byte(answer), &names); err != nil {
		return nil
	}
	return names
}

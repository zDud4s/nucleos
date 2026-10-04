package fetch

import (
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"nucleosemail/daemon"
	"nucleosemail/extract"
)

func request_(authorization string) *http.Request {
	r, _ := http.NewRequest(http.MethodGet, "/attachment?uid=1&position=0", nil)
	if authorization != "" {
		r.Header.Set("Authorization", authorization)
	}
	return r
}

// This listener reads a person's mailbox on demand. Every way of arriving without the daemon's
// token has to be a refusal, including the ones that look almost right.
func TestOnlyTheDaemonsTokenIsAccepted(t *testing.T) {
	for _, header := range []string{
		"",
		"Bearer ",
		"Bearer wrong",
		"Bearer secre",   // a prefix of the real one
		"Bearer secrets", // the real one with more after it
		"secret",         // no scheme
		"bearer secret",  // the scheme is case-sensitive here by choice
		"Basic secret",
	} {
		if authorized(request_(header), "secret") {
			t.Errorf("accepted %q", header)
		}
	}
	if !authorized(request_("Bearer secret"), "secret") {
		t.Error("rejected the real token")
	}
}

func TestRequestRejectsWhatCannotAddressAnAttachment(t *testing.T) {
	for _, query := range []string{
		"",
		"?uid=0&position=0", // uid 0 is not a uid
		"?uid=-1&position=0",
		"?uid=abc&position=0",
		"?uid=1", // no position
		"?uid=1&position=-1",
		"?uid=1&position=x",
	} {
		r, _ := http.NewRequest(http.MethodGet, "/attachment"+query, nil)
		if _, _, err := request(r); err == nil {
			t.Errorf("accepted %q", query)
		}
	}

	r, _ := http.NewRequest(http.MethodGet, "/attachment?uid=8239&position=2", nil)
	uid, position, err := request(r)
	if err != nil || uid != 8239 || position != 2 {
		t.Fatalf("got uid=%d position=%d err=%v, want 8239/2/nil", uid, position, err)
	}
}

func TestATruncatedAttachmentIsA413NamingTheTrueSize(t *testing.T) {
	w := httptest.NewRecorder()
	serveAttachment(w, 1, 0, attachment{Filename: "big.bin", SizeBytes: 40 << 20}, make([]byte, 10), extract.ErrAttachmentTruncated)

	if w.Code != http.StatusRequestEntityTooLarge {
		t.Fatalf("status = %d, want 413", w.Code)
	}
	var body tooLarge
	if err := json.Unmarshal(w.Body.Bytes(), &body); err != nil {
		t.Fatal(err)
	}
	if body.Error != "attachment_too_large" || body.SizeBytes != 40<<20 || body.MaxBytes != extract.MaxAttachmentBytes {
		t.Errorf("body = %+v", body)
	}
	if w.Body.Len() > 200 {
		t.Errorf("the capped bytes leaked into the answer (%d bytes)", w.Body.Len())
	}
}

func TestOtherAttachmentOutcomesAreUnchanged(t *testing.T) {
	w := httptest.NewRecorder()
	serveAttachment(w, 1, 0, attachment{Filename: "a.txt", SizeBytes: 3}, []byte("abc"), nil)
	if w.Code != http.StatusOK || w.Body.String() != "abc" {
		t.Errorf("ok: %d %q", w.Code, w.Body.String())
	}

	w = httptest.NewRecorder()
	serveAttachment(w, 1, 0, attachment{}, nil, extract.ErrNoSuchAttachment)
	if w.Code != http.StatusNotFound {
		t.Errorf("missing: %d", w.Code)
	}

	w = httptest.NewRecorder()
	serveAttachment(w, 1, 0, attachment{}, nil, errors.New("imap down"))
	if w.Code != http.StatusBadGateway {
		t.Errorf("other: %d", w.Code)
	}
}

func TestBulkMarksOnlyTheCutAttachmentsTruncated(t *testing.T) {
	described := []daemon.Attachment{
		{Position: 0, Filename: "small.txt", SizeBytes: 3},
		{Position: 1, Filename: "big.bin", SizeBytes: 40 << 20},
	}
	contents := map[int][]byte{0: []byte("abc"), 1: make([]byte, 5)}

	payload := bulkPayload(described, contents)
	if payload[0].Truncated {
		t.Error("a complete attachment was marked truncated")
	}
	if !payload[1].Truncated || payload[1].SizeBytes != 40<<20 {
		t.Errorf("cut attachment = %+v, want truncated with the true size", payload[1])
	}
	raw, _ := json.Marshal(payload[1])
	if !strings.Contains(string(raw), `"truncated":true`) {
		t.Errorf("wire form lacks the flag: %s", raw)
	}
}

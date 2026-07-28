package fetch

import (
	"net/http"
	"testing"
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

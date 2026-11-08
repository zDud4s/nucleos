package serve

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleosquota/config"
	"nucleosquota/reading"
)

var accTestCfg = config.Config{SuccessTTL: time.Minute, ErrorTTL: time.Second}

func absDir(t *testing.T, name string) string {
	t.Helper()
	return filepath.Join(t.TempDir(), name)
}

func put(t *testing.T, h http.HandlerFunc, method, body string) *httptest.ResponseRecorder {
	t.Helper()
	req := httptest.NewRequest(method, "/accounts", strings.NewReader(body))
	rec := httptest.NewRecorder()
	h(rec, req)
	return rec
}

var seenMu sync.Mutex

func accountReaders(seen *[]Account) Readers {
	return Readers{
		Account: func(ctx context.Context, a Account, now time.Time) reading.Provider {
			seenMu.Lock()
			*seen = append(*seen, a)
			seenMu.Unlock()
			// A window set unlike 5h/7d, to prove nothing here depends on those names.
			return reading.Provider{
				Name: a.Engine, Fidelity: reading.Official, ReadAt: now,
				Windows: []reading.Window{{Name: "1h", UsedFraction: 0.5}, {Name: "30d", UsedFraction: 0.1}},
			}
		},
	}
}

func Test_with_no_account_list_the_legacy_providers_are_read_and_accounts_is_empty(t *testing.T) {
	var cc, xc int
	readers := stubReaders(&cc, &xc, official("claude", time.Now()), official("codex", time.Now()))
	handler := quotaHandler(accTestCfg, readers, &cache{}, time.Now)

	_, body := doGet(t, handler, "")

	if len(body.Providers) != 2 || cc != 1 || xc != 1 {
		t.Errorf("legacy providers = %d (claude=%d codex=%d), want 2/1/1", len(body.Providers), cc, xc)
	}
	if body.Accounts == nil || len(body.Accounts) != 0 {
		t.Errorf("accounts = %v, want empty non-nil", body.Accounts)
	}
}

func Test_a_pushed_list_switches_to_per_account_readings_without_restart(t *testing.T) {
	var seen []Account
	var cc, xc int
	readers := accountReaders(&seen)
	readers.Claude = func(context.Context, time.Time) reading.Provider { cc++; return official("claude", time.Now()) }
	readers.Codex = func(time.Time) reading.Provider { xc++; return official("codex", time.Now()) }
	c := &cache{}
	quota := quotaHandler(accTestCfg, readers, c, time.Now)
	accounts := accountsHandler(c)

	dirA, dirB := absDir(t, "a"), absDir(t, "b")
	list := AccountsRequest{Accounts: []Account{
		{ID: "default:claude", Engine: "claude", Dir: dirA},
		{ID: "work:claude", Engine: "claude", Dir: dirB},
	}}
	raw, _ := json.Marshal(list)

	// Prime the cache with the legacy answer, then push the list: it must not be served stale.
	doGet(t, quota, "")
	if rec := put(t, accounts, http.MethodPut, string(raw)); rec.Code != http.StatusOK {
		t.Fatalf("PUT status = %d (%s)", rec.Code, rec.Body.String())
	}
	_, body := doGet(t, quota, "")

	if body.Cached {
		t.Error("the answer after a list change must be fresh")
	}
	if len(body.Providers) != 0 {
		t.Errorf("providers = %d, want 0 once a list is pushed", len(body.Providers))
	}
	if len(body.Accounts) != 2 || body.Accounts[0].AccountID != "default:claude" || body.Accounts[1].AccountID != "work:claude" {
		t.Fatalf("accounts = %+v", body.Accounts)
	}
	if got := body.Accounts[1].Windows; len(got) != 2 || got[0].Name != "1h" || got[1].Name != "30d" {
		t.Errorf("windows = %+v, want 1h and 30d passed through", got)
	}
	if cc != 1 || xc != 1 {
		t.Errorf("legacy readers ran again after the list was pushed: claude=%d codex=%d", cc, xc)
	}
	dirs := map[string]bool{}
	for _, a := range seen {
		dirs[a.Dir] = true
	}
	if !dirs[dirA] || !dirs[dirB] {
		t.Errorf("readers got dirs %v, want both account dirs", dirs)
	}

	// Shrink the list live.
	raw, _ = json.Marshal(AccountsRequest{Accounts: list.Accounts[:1]})
	put(t, accounts, http.MethodPut, string(raw))
	_, body = doGet(t, quota, "")
	if len(body.Accounts) != 1 || body.Accounts[0].AccountID != "default:claude" {
		t.Errorf("after shrinking, accounts = %+v", body.Accounts)
	}

	// An empty list is a real answer: no accounts, not a fall back to legacy.
	put(t, accounts, http.MethodPut, `{"accounts":[]}`)
	_, body = doGet(t, quota, "")
	if len(body.Accounts) != 0 || len(body.Providers) != 0 || cc != 1 {
		t.Errorf("empty list: accounts=%d providers=%d claudeCalls=%d", len(body.Accounts), len(body.Providers), cc)
	}
}

func Test_an_engine_without_a_reader_is_unmeasured_never_a_guess(t *testing.T) {
	c := &cache{}
	quota := quotaHandler(accTestCfg, Readers{Account: liveAccount(accTestCfg)}, c, time.Now)
	raw, _ := json.Marshal(AccountsRequest{Accounts: []Account{{ID: "x:antigravity", Engine: "antigravity", Dir: absDir(t, "x")}}})
	put(t, accountsHandler(c), http.MethodPut, string(raw))

	_, body := doGet(t, quota, "")

	if len(body.Accounts) != 1 || body.Accounts[0].Fidelity != reading.Unmeasured || len(body.Accounts[0].Windows) != 0 {
		t.Errorf("accounts = %+v, want one unmeasured with no windows", body.Accounts)
	}
}

func Test_an_invalid_list_is_rejected_whole_and_changes_nothing(t *testing.T) {
	d := absDir(t, "a")
	cases := map[string]string{
		"not json":     `nope`,
		"empty id":     `{"accounts":[{"id":"","engine":"claude","dir":"` + jsonPath(d) + `"}]}`,
		"duplicate id": `{"accounts":[{"id":"a","engine":"claude","dir":"` + jsonPath(d) + `"},{"id":"a","engine":"codex","dir":"` + jsonPath(d) + `"}]}`,
		"empty engine": `{"accounts":[{"id":"a","engine":"","dir":"` + jsonPath(d) + `"}]}`,
		"relative dir": `{"accounts":[{"id":"a","engine":"claude","dir":"rel/dir"}]}`,
		"empty dir":    `{"accounts":[{"id":"a","engine":"claude","dir":""}]}`,
	}
	for name, body := range cases {
		t.Run(name, func(t *testing.T) {
			c := &cache{}
			if rec := put(t, accountsHandler(c), http.MethodPut, body); rec.Code != http.StatusBadRequest {
				t.Errorf("status = %d, want 400", rec.Code)
			}
			if c.accountsSet {
				t.Error("a rejected list must not switch the sidecar to account mode")
			}
		})
	}
	if rec := put(t, accountsHandler(&cache{}), http.MethodPost, `{}`); rec.Code != http.StatusMethodNotAllowed {
		t.Errorf("POST status = %d, want 405", rec.Code)
	}
}

func Test_accounts_requires_the_bearer(t *testing.T) {
	h := authorized("tok", accountsHandler(&cache{}))
	if rec := put(t, h, http.MethodPut, `{"accounts":[]}`); rec.Code != http.StatusUnauthorized {
		t.Errorf("status = %d, want 401", rec.Code)
	}
}

// The wire carries ids, engines, fractions and windows. The account directory and anything read
// from it must never appear in a response.
func Test_the_response_never_carries_the_directory(t *testing.T) {
	var seen []Account
	c := &cache{}
	quota := quotaHandler(accTestCfg, accountReaders(&seen), c, time.Now)
	dir := absDir(t, "secret-dir-name")
	raw, _ := json.Marshal(AccountsRequest{Accounts: []Account{{ID: "a", Engine: "claude", Dir: dir}}})
	put(t, accountsHandler(c), http.MethodPut, string(raw))

	rec, _ := doGet(t, quota, "")

	if strings.Contains(rec.Body.String(), "secret-dir-name") {
		t.Errorf("response leaks the account dir: %s", rec.Body.String())
	}
}

func jsonPath(p string) string {
	b, _ := json.Marshal(p)
	return strings.Trim(string(b), `"`)
}

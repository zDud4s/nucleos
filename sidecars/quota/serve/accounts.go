package serve

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"path/filepath"
	"sync"
	"time"

	"nucleosquota/claude"
	"nucleosquota/codex"
	"nucleosquota/config"
	"nucleosquota/reading"
)

// Account is one pool account the núcleo wants read: a stable id, the engine that decides which
// reader applies, and the account's config directory (CLAUDE_CONFIG_DIR for claude, CODEX_HOME for
// codex). The directory holds the credential; it is read here and never sent back.
type Account struct {
	ID     string `json:"id"`
	Engine string `json:"engine"`
	Dir    string `json:"dir"`
	// Token is an optional Claude subscription token (from `claude setup-token`). When set for a
	// claude account it is used instead of the credentials file in Dir. It is a secret: it is never
	// echoed, and Format below keeps it out of any fmt rendering of the struct.
	Token string `json:"token,omitempty"`
}

// Format renders an Account without its token under every fmt verb (%v, %+v, %#v, %s), so a stray
// log line or error cannot leak it. The directory stays, as before.
func (a Account) Format(f fmt.State, _ rune) {
	redacted := ""
	if a.Token != "" {
		redacted = "[redacted]"
	}
	fmt.Fprintf(f, "Account{ID:%q Engine:%q Dir:%q Token:%q}", a.ID, a.Engine, a.Dir, redacted)
}

// AccountsRequest is the body of PUT /accounts: the complete list of active accounts. Each call
// replaces the previous list, so the núcleo sends it at start and whenever it changes.
type AccountsRequest struct {
	Accounts []Account `json:"accounts"`
}

// AccountsResponse acknowledges a PUT /accounts.
type AccountsResponse struct {
	Accepted int `json:"accepted"`
}

// maxAccountsBody bounds the request: a list of accounts is a few hundred bytes.
const maxAccountsBody = 1 << 20

// validate rejects a list the sidecar cannot act on, before any of it is applied. An engine this
// sidecar has no reader for is NOT an error here: it is read as Unmeasured, because the pool may
// grow engines faster than this process, and "no source" is an honest answer (design R4).
func validate(accounts []Account) error {
	seen := make(map[string]bool, len(accounts))
	for i, a := range accounts {
		switch {
		case a.ID == "":
			return fmt.Errorf("accounts[%d]: id is empty", i)
		case seen[a.ID]:
			return fmt.Errorf("accounts[%d]: duplicate id %q", i, a.ID)
		case a.Engine == "":
			return fmt.Errorf("accounts[%d]: engine is empty", i)
		case a.Dir == "" || !filepath.IsAbs(a.Dir):
			return fmt.Errorf("accounts[%d]: dir must be an absolute path", i)
		}
		seen[a.ID] = true
	}
	return nil
}

// accountsHandler replaces the account list and drops the cached answer, so the next GET /quota
// reads the new set instead of serving the old one for up to a TTL.
func accountsHandler(c *cache) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPut {
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		var req AccountsRequest
		dec := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxAccountsBody))
		if err := dec.Decode(&req); err != nil {
			http.Error(w, "body is not the JSON we expect", http.StatusBadRequest)
			return
		}
		if err := validate(req.Accounts); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}

		c.mu.Lock()
		c.accounts = append([]Account{}, req.Accounts...)
		c.accountsSet = true
		c.answer = nil
		c.mu.Unlock()

		writeJSON(w, AccountsResponse{Accepted: len(req.Accounts)})
	}
}

// readAccounts reads every account concurrently (each is a separate credential and, for claude, a
// separate outbound request), keeping the list's order in the answer.
func readAccounts(ctx context.Context, readers Readers, accounts []Account, now time.Time) []reading.Provider {
	out := make([]reading.Provider, len(accounts))
	var wg sync.WaitGroup
	for i, a := range accounts {
		wg.Add(1)
		go func(i int, a Account) {
			defer wg.Done()
			var p reading.Provider
			if readers.Account == nil {
				p = reading.Unavailable(a.Engine, "no usage source for this engine", now)
			} else {
				p = readers.Account(ctx, a, now)
			}
			p.AccountID = a.ID
			if p.Name == "" {
				p.Name = a.Engine
			}
			out[i] = p
		}(i, a)
	}
	wg.Wait()
	return out
}

// Reader constructors, variables only so tests can aim them at an httptest server.
var (
	newDirReader   = claude.NewForDir
	newTokenReader = claude.NewForToken
)

// liveAccount builds the reader for one account from its engine and directory.
func liveAccount(cfg config.Config) func(context.Context, Account, time.Time) reading.Provider {
	return func(ctx context.Context, a Account, now time.Time) reading.Provider {
		switch a.Engine {
		case claude.Name:
			if a.Token != "" {
				return newTokenReader(cfg.FetchTimeout, a.Token).Read(ctx, now)
			}
			return newDirReader(cfg.FetchTimeout, a.Dir).Read(ctx, now)
		case codex.Name:
			return codex.NewForDir(a.Dir).Read(now)
		default:
			return reading.Unavailable(a.Engine, "no usage source for this engine", now)
		}
	}
}

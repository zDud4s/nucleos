package serve

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"sync"
	"time"

	"github.com/go-webauthn/webauthn/protocol"
	"github.com/go-webauthn/webauthn/webauthn"

	"nucleosremote/config"
)

// The passkey ceremonies of an owner device (step-up for grave actions).
//
// The ceremony runs here, in the gateway, because the gateway already knows the HTTPS origin the
// browser sees; the daemon only stores credentials and opens the step-up window. Everything the
// daemon is asked goes through its gateway-only routes (the gateway's key, no device session):
//
//	GET  /remote/passkeys/{device}   the device's credentials and standing
//	POST /remote/passkeys            store a new credential, or replace one after a verify
//	POST /remote/step-up             open the device's step-up window
//
// Who the device is comes from the daemon's answer to GET /remote/session for the browser's own
// cookie, never from the request body. The relying party id and the origin come from the configured
// public origin, never from the Host or Origin headers of a request.

const (
	passkeyBodyMax = 32 << 10
	// ceremonyTTL is how long a challenge lives, and the timeout the browser is told.
	ceremonyTTL = 5 * time.Minute
	// maxCeremonies bounds the memory a flood of begin calls can take.
	maxCeremonies          = 256
	passkeyDeviceLimit     = 20
	passkeyGlobalLimit     = 120
	passkeyFailLimit       = 8
	maxCeremoniesPerDevice = 8
)

// refusalPairingWindowClosed: the first passkey is only registered within 15 minutes of the
// device's pairing.
const refusalPairingWindowClosed = "pairing_window_closed"

// Refusal words of the gateway's passkey surface.
const (
	refusalNotOwner       = "not_an_owner"
	refusalStepUpRequired = "step_up_required"
	refusalNoPasskey      = "no_passkey"
	refusalCeremony       = "ceremony_expired"
	refusalPasskey        = "passkey_refused"
)

// ceremony is one begun, unfinished registration or verification.
type ceremony struct {
	session  webauthn.SessionData
	deviceID int64
	expires  time.Time
}

// ceremonyStore keeps the begun ceremonies in memory: a challenge is meant to die with the process.
// A ceremony is keyed by its kind and the hash of the session cookie that began it, so another
// session cannot finish it, and there is at most one per kind per session.
type ceremonyStore struct {
	mu  sync.Mutex
	now func() time.Time
	m   map[string]ceremony
}

func newCeremonyStore() *ceremonyStore {
	return &ceremonyStore{now: time.Now, m: map[string]ceremony{}}
}

func ceremonyKey(kind, secret string) string {
	sum := sha256.Sum256([]byte(secret))
	return kind + ":" + hex.EncodeToString(sum[:])
}

func (s *ceremonyStore) put(key string, c ceremony) {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.now()
	for k, v := range s.m {
		if !now.Before(v.expires) {
			delete(s.m, k)
		}
	}
	if _, replaces := s.m[key]; !replaces {
		count := 0
		var oldest string
		var oldestExpiry time.Time
		for k, v := range s.m {
			if v.deviceID != c.deviceID {
				continue
			}
			count++
			if oldest == "" || v.expires.Before(oldestExpiry) {
				oldest, oldestExpiry = k, v.expires
			}
		}
		if count >= maxCeremoniesPerDevice {
			delete(s.m, oldest)
		}
	}
	if len(s.m) >= maxCeremonies {
		for k := range s.m {
			delete(s.m, k)
			break
		}
	}
	c.expires = now.Add(ceremonyTTL)
	s.m[key] = c
}

// take removes and returns the ceremony: a challenge is single use, whether or not the finish that
// consumed it succeeds.
func (s *ceremonyStore) take(key string) (ceremony, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	c, ok := s.m[key]
	delete(s.m, key)
	if !ok || !s.now().Before(c.expires) {
		return ceremony{}, false
	}
	return c, true
}

// passkeyUser is the webauthn user of one device: a stable handle derived from the device id and
// only that device's credentials.
type passkeyUser struct {
	id    int64
	name  string
	creds []webauthn.Credential
}

func (u passkeyUser) WebAuthnID() []byte {
	sum := sha256.Sum256([]byte(fmt.Sprintf("nucleos-remote-device:%d", u.id)))
	return sum[:]
}
func (u passkeyUser) WebAuthnName() string                       { return u.name }
func (u passkeyUser) WebAuthnDisplayName() string                { return u.name }
func (u passkeyUser) WebAuthnCredentials() []webauthn.Credential { return u.creds }

// newRelyingParty builds the webauthn library from the configured public origin alone.
func newRelyingParty(cfg config.Config) (*webauthn.WebAuthn, error) {
	u, err := url.Parse(cfg.PublicOrigin)
	if err != nil || u.Hostname() == "" {
		return nil, fmt.Errorf("public origin: not a valid URL")
	}
	return webauthn.New(&webauthn.Config{
		RPID:          u.Hostname(),
		RPDisplayName: "NucleOS",
		RPOrigins:     []string{cfg.PublicOrigin},
		Timeouts: webauthn.TimeoutsConfig{
			Login:        webauthn.TimeoutConfig{Enforce: true, Timeout: ceremonyTTL, TimeoutUVD: ceremonyTTL},
			Registration: webauthn.TimeoutConfig{Enforce: true, Timeout: ceremonyTTL, TimeoutUVD: ceremonyTTL},
		},
	})
}

// callerDevice is who the browser's cookie says it is, as the daemon answered.
type callerDevice struct {
	secret string
	id     int64
	name   string
	owner  bool
}

// storedPasskey is one row the daemon keeps for a device.
type storedPasskey struct {
	RowID      int64  `json:"id"`
	Credential string `json:"credential"`
}

// passkeyState is the daemon's answer to GET /remote/passkeys/{device}.
type passkeyState struct {
	Credentials []storedPasskey `json:"credentials"`
	Owner       bool            `json:"owner"`
	SteppedUp   bool            `json:"stepped_up"`
	CanRegister bool            `json:"can_register"`
}

// passkeyRoute dispatches the browser's passkey and ticket ceremonies. It reports false for any other path.
func (a *authHandler) passkeyRoute(w http.ResponseWriter, r *http.Request) bool {
	var h func(http.ResponseWriter, *http.Request)
	method := http.MethodPost
	switch r.URL.Path {
	case "/auth/stepup/info":
		h, method = a.ticketInfo, http.MethodGet
	case "/auth/stepup/begin":
		h = a.ticketBegin
	case "/auth/stepup/finish":
		h = a.ticketFinish
	case "/auth/passkey/status":
		h, method = a.passkeyStatus, http.MethodGet
	case "/auth/passkey/register/begin":
		h = a.registerBegin
	case "/auth/passkey/register/finish":
		h = a.registerFinish
	case "/auth/passkey/verify/begin":
		h = a.verifyBegin
	case "/auth/passkey/verify/finish":
		h = a.verifyFinish
	default:
		return false
	}
	if r.Method != method {
		writeJSONError(w, http.StatusMethodNotAllowed, "method_not_allowed")
		return true
	}
	if a.wa == nil {
		writeJSONError(w, http.StatusServiceUnavailable, "passkeys_unavailable")
		return true
	}
	if !a.globalAllowed(w, r.URL.Path) {
		return true
	}
	h(w, r)
	return true
}

type ticketState struct {
	DeviceID    int64           `json:"device_id"`
	DeviceName  string          `json:"device_name"`
	Label       string          `json:"label"`
	Code        string          `json:"code"`
	Credentials []storedPasskey `json:"credentials"`
	CanRegister bool            `json:"can_register"`
}

func ticketInvalid(w http.ResponseWriter) {
	writeJSONError(w, http.StatusGone, "stepup_ticket_invalid")
}

func (a *authHandler) ticketState(ctx context.Context, w http.ResponseWriter, ticket string) (ticketState, bool) {
	if ticket == "" {
		ticketInvalid(w)
		return ticketState{}, false
	}
	status, data, err := a.d.call(ctx, http.MethodGet, "/remote/step-up/ticket/"+url.PathEscape(ticket), nil, "")
	if err != nil {
		writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
		return ticketState{}, false
	}
	if status == http.StatusGone || status == http.StatusNotFound {
		ticketInvalid(w)
		return ticketState{}, false
	}
	var out ticketState
	if status != http.StatusOK || json.Unmarshal(data, &out) != nil || out.DeviceID <= 0 {
		writeJSONError(w, http.StatusBadGateway, "daemon_error")
		return ticketState{}, false
	}
	if !a.deviceAllowed(w, out.DeviceID, "/auth/stepup") {
		return ticketState{}, false
	}
	return out, true
}

// ticketInfo only reads metadata. It neither starts nor consumes a ceremony, so a prefetched link is inert.
func (a *authHandler) ticketInfo(w http.ResponseWriter, r *http.Request) {
	info, ok := a.ticketState(r.Context(), w, r.URL.Query().Get("ticket"))
	if !ok {
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"device_name": info.DeviceName, "label": info.Label, "code": info.Code, "registered": len(info.Credentials) > 0, "can_register": info.CanRegister})
}

func (a *authHandler) ticketBegin(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Ticket string `json:"ticket"`
	}
	if json.NewDecoder(http.MaxBytesReader(w, r.Body, passkeyBodyMax)).Decode(&in) != nil {
		writeJSONError(w, http.StatusBadRequest, "bad_request")
		return
	}
	st, ok := a.ticketState(r.Context(), w, in.Ticket)
	if !ok {
		return
	}
	dev := callerDevice{id: st.DeviceID, name: st.DeviceName, owner: true}
	user, _ := userOf(dev, passkeyState{Credentials: st.Credentials, CanRegister: st.CanRegister})
	if len(user.creds) == 0 {
		if !st.CanRegister {
			writeJSONError(w, http.StatusForbidden, refusalPairingWindowClosed)
			return
		}
		exclude := make([]protocol.CredentialDescriptor, 0)
		creation, session, err := a.wa.BeginRegistration(user, webauthn.WithExclusions(exclude), webauthn.WithAuthenticatorSelection(protocol.AuthenticatorSelection{UserVerification: protocol.VerificationRequired}))
		if err != nil {
			writeJSONError(w, http.StatusInternalServerError, "passkey_error")
			return
		}
		a.cer.put(ceremonyKey("ticket-register", in.Ticket), ceremony{session: *session, deviceID: dev.id})
		writeJSON(w, http.StatusOK, map[string]any{"kind": "register", "publicKey": creation})
		return
	}
	assertion, session, err := a.wa.BeginLogin(user, webauthn.WithUserVerification(protocol.VerificationRequired))
	if err != nil {
		writeJSONError(w, http.StatusInternalServerError, "passkey_error")
		return
	}
	a.cer.put(ceremonyKey("ticket-verify", in.Ticket), ceremony{session: *session, deviceID: dev.id})
	writeJSON(w, http.StatusOK, map[string]any{"kind": "verify", "publicKey": assertion})
}

func (a *authHandler) ticketFinish(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Ticket     string          `json:"ticket"`
		Credential json.RawMessage `json:"credential"`
	}
	if json.NewDecoder(http.MaxBytesReader(w, r.Body, passkeyBodyMax)).Decode(&in) != nil || len(in.Credential) == 0 {
		writeJSONError(w, http.StatusBadRequest, "bad_request")
		return
	}
	st, ok := a.ticketState(r.Context(), w, in.Ticket)
	if !ok {
		return
	}
	dev := callerDevice{id: st.DeviceID, name: st.DeviceName, owner: true}
	failed := true
	defer func() {
		if failed {
			a.finishFailure(dev.id)
		}
	}()
	verify, hasVerify := a.cer.take(ceremonyKey("ticket-verify", in.Ticket))
	register, hasRegister := a.cer.take(ceremonyKey("ticket-register", in.Ticket))
	if (!hasVerify && !hasRegister) || (hasVerify && hasRegister) {
		writeJSONError(w, http.StatusBadRequest, refusalCeremony)
		return
	}
	if hasVerify {
		parsed, err := protocol.ParseCredentialRequestResponseBytes(in.Credential)
		if err != nil {
			writeJSONError(w, http.StatusBadRequest, refusalPasskey)
			return
		}
		user, rows := userOf(dev, passkeyState{Credentials: st.Credentials})
		cred, err := a.wa.ValidateLogin(user, verify.session, parsed)
		if err != nil {
			writeJSONError(w, http.StatusBadRequest, refusalPasskey)
			return
		}
		row, known := rows[string(cred.ID)]
		if cred.Authenticator.CloneWarning || !known {
			writeJSONError(w, http.StatusBadRequest, refusalPasskey)
			return
		}
		stored, _ := json.Marshal(cred)
		credential, _ := json.Marshal(map[string]any{"device_id": dev.id, "credential_json": string(stored), "replaces": row})
		if !a.consumeTicketBeforeCredential(w, r, in.Ticket, credential) {
			return
		}
	} else {
		parsed, err := protocol.ParseCredentialCreationResponseBytes(in.Credential)
		if err != nil {
			writeJSONError(w, http.StatusBadRequest, refusalPasskey)
			return
		}
		cred, err := a.wa.CreateCredential(passkeyUser{id: dev.id, name: dev.name}, register.session, parsed)
		if err != nil {
			writeJSONError(w, http.StatusBadRequest, refusalPasskey)
			return
		}
		stored, _ := json.Marshal(cred)
		credential, _ := json.Marshal(map[string]any{"device_id": dev.id, "credential_json": string(stored)})
		if !a.consumeTicketBeforeCredential(w, r, in.Ticket, credential) {
			return
		}
	}
	a.finishSuccess(dev.id)
	failed = false
	writeJSON(w, http.StatusOK, map[string]any{"stepped_up": true})
}

// consumeTicketBeforeCredential burns the one-time ticket before changing a credential. A failed
// consume therefore cannot leave a credential change that acquired no step-up.
func (a *authHandler) consumeTicketBeforeCredential(w http.ResponseWriter, r *http.Request, ticket string, credential []byte) bool {
	consume, _ := json.Marshal(map[string]any{"ticket": ticket})
	status, data, err := a.d.call(r.Context(), http.MethodPost, "/remote/step-up/ticket/consume", consume, "")
	if err != nil {
		writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
		return false
	}
	if status == http.StatusGone || status == http.StatusNotFound {
		ticketInvalid(w)
		return false
	}
	if status != http.StatusOK {
		a.daemonRefusal(w, status, data)
		return false
	}
	status, data, err = a.d.call(r.Context(), http.MethodPost, "/remote/passkeys", credential, "")
	if err != nil {
		writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
		return false
	}
	if status != http.StatusOK && status != http.StatusCreated {
		a.daemonRefusal(w, status, data)
		return false
	}
	return true
}

// caller asks the daemon who the cookie is. On any failure it has already answered and says false.
func (a *authHandler) caller(w http.ResponseWriter, r *http.Request) (callerDevice, bool) {
	secret, ok := requestSession(r)
	if !ok {
		a.sessionLost(w)
		return callerDevice{}, false
	}
	status, data, err := a.d.call(r.Context(), http.MethodGet, "/remote/session", nil, secret)
	if err != nil {
		writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
		return callerDevice{}, false
	}
	if status == http.StatusUnauthorized {
		if refusalOf(data) == refusalSessionInvalid {
			a.sessionLost(w)
		} else {
			writeJSONError(w, http.StatusBadGateway, "gateway_credential_rejected")
		}
		return callerDevice{}, false
	}
	var info struct {
		DeviceID int64  `json:"device_id"`
		Name     string `json:"name"`
		Owner    bool   `json:"owner"`
	}
	if status != http.StatusOK || json.Unmarshal(data, &info) != nil || info.DeviceID <= 0 {
		writeJSONError(w, http.StatusBadGateway, "daemon_error")
		return callerDevice{}, false
	}
	if !a.deviceAllowed(w, info.DeviceID, r.URL.Path) {
		return callerDevice{}, false
	}
	return callerDevice{secret: secret, id: info.DeviceID, name: info.Name, owner: info.Owner}, true
}

// ownerCaller is caller plus the owner check every passkey call needs.
func (a *authHandler) ownerCaller(w http.ResponseWriter, r *http.Request) (callerDevice, bool) {
	dev, ok := a.caller(w, r)
	if !ok {
		return dev, false
	}
	if !dev.owner {
		writeJSONError(w, http.StatusForbidden, refusalNotOwner)
		return dev, false
	}
	return dev, true
}

// state reads the device's credentials and standing from the daemon.
func (a *authHandler) state(ctx context.Context, w http.ResponseWriter, id int64) (passkeyState, bool) {
	status, data, err := a.d.call(ctx, http.MethodGet, fmt.Sprintf("/remote/passkeys/%d", id), nil, "")
	if err != nil {
		writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
		return passkeyState{}, false
	}
	var st passkeyState
	if status != http.StatusOK || json.Unmarshal(data, &st) != nil {
		a.daemonRefusal(w, status, data)
		return passkeyState{}, false
	}
	return st, true
}

// daemonRefusal turns a refusal of a gateway-only daemon route into the browser's answer. Only the
// words this surface defines pass through; anything else is the gateway's fault, not the person's.
func (a *authHandler) daemonRefusal(w http.ResponseWriter, status int, data []byte) {
	switch word := refusalOf(data); {
	case status == http.StatusForbidden && (word == refusalNotOwner || word == refusalStepUpRequired || word == refusalNoPasskey || word == refusalPairingWindowClosed):
		writeJSONError(w, http.StatusForbidden, word)
	case status == http.StatusNotFound:
		a.sessionLost(w)
	case status == http.StatusUnauthorized:
		writeJSONError(w, http.StatusBadGateway, "gateway_credential_rejected")
	default:
		writeJSONError(w, http.StatusBadGateway, "daemon_error")
	}
}

// userOf builds the webauthn user from the daemon's rows. A row that does not parse is skipped: it
// cannot match an assertion, and the others still work.
func userOf(dev callerDevice, st passkeyState) (passkeyUser, map[string]int64) {
	user := passkeyUser{id: dev.id, name: dev.name}
	rows := map[string]int64{}
	for _, row := range st.Credentials {
		var c webauthn.Credential
		if json.Unmarshal([]byte(row.Credential), &c) != nil || len(c.ID) == 0 {
			continue
		}
		user.creds = append(user.creds, c)
		rows[string(c.ID)] = row.RowID
	}
	return user, rows
}

func (a *authHandler) passkeyStatus(w http.ResponseWriter, r *http.Request) {
	dev, ok := a.caller(w, r)
	if !ok {
		return
	}
	if !dev.owner {
		writeJSON(w, http.StatusOK, map[string]any{"owner": false, "registered": false, "stepped_up": false, "can_register": false})
		return
	}
	st, ok := a.state(r.Context(), w, dev.id)
	if !ok {
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"owner":        true,
		"registered":   len(st.Credentials) > 0,
		"stepped_up":   st.SteppedUp,
		"can_register": st.CanRegister,
	})
}

func (a *authHandler) registerBegin(w http.ResponseWriter, r *http.Request) {
	dev, ok := a.ownerCaller(w, r)
	if !ok {
		return
	}
	st, ok := a.state(r.Context(), w, dev.id)
	if !ok {
		return
	}
	// A device that already has a passkey adds another only inside a step-up, or a stolen owner
	// session could enrol its own and defeat the step-up. The daemon enforces it again on store.
	// A device with none yet that cannot register has missed its first-passkey window instead.
	if !st.CanRegister {
		if len(st.Credentials) == 0 {
			writeJSONError(w, http.StatusForbidden, refusalPairingWindowClosed)
			return
		}
		writeJSONError(w, http.StatusForbidden, refusalStepUpRequired)
		return
	}
	user, _ := userOf(dev, st)
	exclude := make([]protocol.CredentialDescriptor, 0, len(user.creds))
	for _, c := range user.creds {
		exclude = append(exclude, c.Descriptor())
	}
	creation, session, err := a.wa.BeginRegistration(user,
		webauthn.WithExclusions(exclude),
		webauthn.WithAuthenticatorSelection(protocol.AuthenticatorSelection{
			UserVerification: protocol.VerificationRequired,
		}),
	)
	if err != nil {
		writeJSONError(w, http.StatusInternalServerError, "passkey_error")
		return
	}
	a.cer.put(ceremonyKey("register", dev.secret), ceremony{session: *session, deviceID: dev.id})
	writeJSON(w, http.StatusOK, creation)
}

// readCeremony reads the finishing body and the ceremony it answers.
func (a *authHandler) readCeremony(w http.ResponseWriter, r *http.Request, kind string, dev callerDevice) ([]byte, ceremony, bool) {
	// The ceremony is consumed before the body is read: a malformed finish burns it too.
	c, found := a.cer.take(ceremonyKey(kind, dev.secret))
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, passkeyBodyMax))
	if err != nil {
		writeJSONError(w, http.StatusBadRequest, "bad_request")
		return nil, ceremony{}, false
	}
	if !found || c.deviceID != dev.id {
		writeJSONError(w, http.StatusBadRequest, refusalCeremony)
		return nil, ceremony{}, false
	}
	return body, c, true
}

func (a *authHandler) registerFinish(w http.ResponseWriter, r *http.Request) {
	dev, ok := a.ownerCaller(w, r)
	if !ok {
		return
	}
	failed := true
	defer func() {
		if failed {
			a.finishFailure(dev.id)
		}
	}()
	body, c, ok := a.readCeremony(w, r, "register", dev)
	if !ok {
		return
	}
	parsed, err := protocol.ParseCredentialCreationResponseBytes(body)
	if err != nil {
		writeJSONError(w, http.StatusBadRequest, refusalPasskey)
		return
	}
	cred, err := a.wa.CreateCredential(passkeyUser{id: dev.id, name: dev.name}, c.session, parsed)
	if err != nil {
		writeJSONError(w, http.StatusBadRequest, refusalPasskey)
		return
	}
	stored, _ := json.Marshal(cred)
	payload, _ := json.Marshal(map[string]any{"device_id": dev.id, "credential_json": string(stored)})
	status, data, err := a.d.call(r.Context(), http.MethodPost, "/remote/passkeys", payload, "")
	if err != nil {
		writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
		return
	}
	if status != http.StatusOK && status != http.StatusCreated {
		a.daemonRefusal(w, status, data)
		return
	}
	a.finishSuccess(dev.id)
	failed = false
	writeJSON(w, http.StatusOK, map[string]any{"registered": true})
}

func (a *authHandler) verifyBegin(w http.ResponseWriter, r *http.Request) {
	dev, ok := a.ownerCaller(w, r)
	if !ok {
		return
	}
	st, ok := a.state(r.Context(), w, dev.id)
	if !ok {
		return
	}
	user, _ := userOf(dev, st)
	if len(user.creds) == 0 {
		writeJSONError(w, http.StatusConflict, refusalNoPasskey)
		return
	}
	assertion, session, err := a.wa.BeginLogin(user, webauthn.WithUserVerification(protocol.VerificationRequired))
	if err != nil {
		writeJSONError(w, http.StatusInternalServerError, "passkey_error")
		return
	}
	a.cer.put(ceremonyKey("verify", dev.secret), ceremony{session: *session, deviceID: dev.id})
	writeJSON(w, http.StatusOK, assertion)
}

func (a *authHandler) verifyFinish(w http.ResponseWriter, r *http.Request) {
	dev, ok := a.ownerCaller(w, r)
	if !ok {
		return
	}
	failed := true
	defer func() {
		if failed {
			a.finishFailure(dev.id)
		}
	}()
	body, c, ok := a.readCeremony(w, r, "verify", dev)
	if !ok {
		return
	}
	parsed, err := protocol.ParseCredentialRequestResponseBytes(body)
	if err != nil {
		writeJSONError(w, http.StatusBadRequest, refusalPasskey)
		return
	}
	// Only this device's credentials are consulted: another device's passkey cannot open this step-up.
	st, ok := a.state(r.Context(), w, dev.id)
	if !ok {
		return
	}
	user, rows := userOf(dev, st)
	cred, err := a.wa.ValidateLogin(user, c.session, parsed)
	if err != nil || cred.Authenticator.CloneWarning {
		writeJSONError(w, http.StatusBadRequest, refusalPasskey)
		return
	}
	row, known := rows[string(cred.ID)]
	if !known {
		writeJSONError(w, http.StatusBadRequest, refusalPasskey)
		return
	}
	// The new sign count is stored before the window opens: a credential whose counter cannot be
	// recorded would let a clone replay undetected.
	stored, _ := json.Marshal(cred)
	update, _ := json.Marshal(map[string]any{"device_id": dev.id, "credential_json": string(stored), "replaces": row})
	status, data, err := a.d.call(r.Context(), http.MethodPost, "/remote/passkeys", update, "")
	if err != nil {
		writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
		return
	}
	if status != http.StatusOK && status != http.StatusCreated {
		a.daemonRefusal(w, status, data)
		return
	}
	grant, _ := json.Marshal(map[string]any{"device_id": dev.id})
	status, data, err = a.d.call(r.Context(), http.MethodPost, "/remote/step-up", grant, "")
	if err != nil {
		writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
		return
	}
	if status != http.StatusOK {
		a.daemonRefusal(w, status, data)
		return
	}
	var out struct {
		Seconds int `json:"window_seconds"`
	}
	_ = json.Unmarshal(bytes.TrimSpace(data), &out)
	a.finishSuccess(dev.id)
	failed = false
	writeJSON(w, http.StatusOK, map[string]any{"stepped_up": true, "window_seconds": out.Seconds})
}

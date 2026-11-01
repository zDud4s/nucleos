package serve

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"
)

var b64 = base64.RawURLEncoding

// virtualAuthenticator is a software ES256 authenticator with user verification, enough to run the
// ceremonies end to end. origin is what it puts in clientDataJSON, so a test can lie with it.
type virtualAuthenticator struct {
	key     *ecdsa.PrivateKey
	credID  []byte
	counter uint32
	origin  string
}

func newVirtualAuthenticator(origin string) *virtualAuthenticator {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		panic(err)
	}
	id := make([]byte, 32)
	_, _ = rand.Read(id)
	return &virtualAuthenticator{key: key, credID: id, origin: origin}
}

func rpHash(rpID string) []byte {
	sum := sha256.Sum256([]byte(rpID))
	return sum[:]
}

// create answers the `publicKey` object of a registration with a "none"-attested credential.
func (v *virtualAuthenticator) create(t *testing.T, options string) string {
	t.Helper()
	var o struct {
		PublicKey struct {
			Challenge string `json:"challenge"`
			RP        struct {
				ID string `json:"id"`
			} `json:"rp"`
		} `json:"publicKey"`
	}
	if err := json.Unmarshal([]byte(options), &o); err != nil || o.PublicKey.Challenge == "" {
		t.Fatalf("creation options: %v: %s", err, options)
	}
	client, _ := json.Marshal(map[string]any{"type": "webauthn.create", "challenge": o.PublicKey.Challenge, "origin": v.origin})
	cose, _ := cbor.Marshal(map[int]any{
		1: 2, 3: -7, -1: 1,
		-2: v.key.PublicKey.X.FillBytes(make([]byte, 32)),
		-3: v.key.PublicKey.Y.FillBytes(make([]byte, 32)),
	})
	auth := append(rpHash(o.PublicKey.RP.ID), 0x45) // UP | UV | AT
	auth = binary.BigEndian.AppendUint32(auth, v.counter)
	auth = append(auth, make([]byte, 16)...) // AAGUID
	auth = binary.BigEndian.AppendUint16(auth, uint16(len(v.credID)))
	auth = append(auth, v.credID...)
	auth = append(auth, cose...)
	att, _ := cbor.Marshal(map[string]any{"fmt": "none", "attStmt": map[string]any{}, "authData": auth})
	return fmt.Sprintf(`{"id":%q,"rawId":%q,"type":"public-key","response":{"attestationObject":%q,"clientDataJSON":%q}}`,
		b64.EncodeToString(v.credID), b64.EncodeToString(v.credID), b64.EncodeToString(att), b64.EncodeToString(client))
}

// get answers an assertion request, signing with the next counter value (counter is bumped).
func (v *virtualAuthenticator) get(t *testing.T, options string, bump bool) string {
	t.Helper()
	var o struct {
		PublicKey struct {
			Challenge string `json:"challenge"`
			RPID      string `json:"rpId"`
		} `json:"publicKey"`
	}
	if err := json.Unmarshal([]byte(options), &o); err != nil || o.PublicKey.Challenge == "" {
		t.Fatalf("request options: %v: %s", err, options)
	}
	if bump {
		v.counter++
	}
	client, _ := json.Marshal(map[string]any{"type": "webauthn.get", "challenge": o.PublicKey.Challenge, "origin": v.origin})
	auth := append(rpHash(o.PublicKey.RPID), 0x05) // UP | UV
	auth = binary.BigEndian.AppendUint32(auth, v.counter)
	cdh := sha256.Sum256(client)
	digest := sha256.Sum256(append(append([]byte{}, auth...), cdh[:]...))
	sig, err := ecdsa.SignASN1(rand.Reader, v.key, digest[:])
	if err != nil {
		t.Fatal(err)
	}
	return fmt.Sprintf(`{"id":%q,"rawId":%q,"type":"public-key","response":{"authenticatorData":%q,"clientDataJSON":%q,"signature":%q}}`,
		b64.EncodeToString(v.credID), b64.EncodeToString(v.credID), b64.EncodeToString(auth), b64.EncodeToString(client), b64.EncodeToString(sig))
}

// passkeyDaemon is a fake daemon that applies the same rules as the real gateway-only routes.
type passkeyDaemon struct {
	mu       sync.Mutex
	sessions map[string]int64 // session secret -> device id
	owners   map[int64]bool
	stepped  map[int64]bool
	// windowClosed marks a device paired 15 minutes ago or more (the first-passkey window is over).
	windowClosed map[int64]bool
	rows         map[int64][]storedPasskey
	nextRow      int64
	stepUps      int
	seenKey      []string
}

func newPasskeyDaemon() *passkeyDaemon {
	return &passkeyDaemon{
		sessions: map[string]int64{}, owners: map[int64]bool{}, stepped: map[int64]bool{},
		windowClosed: map[int64]bool{}, rows: map[int64][]storedPasskey{}, nextRow: 1,
	}
}

func (p *passkeyDaemon) addDevice(session string, id int64, owner bool) {
	p.sessions[session], p.owners[id] = id, owner
}

func (p *passkeyDaemon) handler(t *testing.T) http.HandlerFunc {
	reply := func(w http.ResponseWriter, status int, v any) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_ = json.NewEncoder(w).Encode(v)
	}
	return func(w http.ResponseWriter, r *http.Request) {
		p.mu.Lock()
		defer p.mu.Unlock()
		if r.Header.Get("Authorization") != "Bearer "+g2Key {
			reply(w, 401, map[string]string{"refusal": "no_key"})
			return
		}
		path := r.URL.Path
		if path == "/remote/session" {
			id, ok := p.sessions[r.Header.Get(DaemonSessionHeader)]
			if !ok {
				reply(w, 401, map[string]string{"refusal": "session_invalid"})
				return
			}
			reply(w, 200, map[string]any{"device_id": id, "name": "laptop", "access_level": "admin", "owner": p.owners[id], "expires_at": "x"})
			return
		}
		// The gateway-only routes carry no device session.
		if r.Header.Get(DaemonSessionHeader) != "" {
			t.Errorf("%s carried a device session", path)
		}
		switch {
		case r.Method == "GET" && strings.HasPrefix(path, "/remote/passkeys/"):
			id, _ := strconv.ParseInt(strings.TrimPrefix(path, "/remote/passkeys/"), 10, 64)
			reply(w, 200, map[string]any{
				"credentials": append([]storedPasskey{}, p.rows[id]...),
				"owner":       p.owners[id], "stepped_up": p.stepped[id],
				"can_register": p.owners[id] && ((len(p.rows[id]) == 0 && !p.windowClosed[id]) || p.stepped[id]),
			})
		case r.Method == "POST" && path == "/remote/passkeys":
			var in struct {
				DeviceID int64  `json:"device_id"`
				Cred     string `json:"credential_json"`
				Replaces int64  `json:"replaces"`
			}
			_ = json.NewDecoder(r.Body).Decode(&in)
			if in.Replaces != 0 {
				for i, row := range p.rows[in.DeviceID] {
					if row.RowID == in.Replaces {
						p.rows[in.DeviceID][i].Credential = in.Cred
						reply(w, 200, map[string]bool{"ok": true})
						return
					}
				}
				reply(w, 404, map[string]string{"refusal": "passkey_not_found"})
				return
			}
			if !p.owners[in.DeviceID] {
				reply(w, 403, map[string]string{"refusal": refusalNotOwner})
				return
			}
			if len(p.rows[in.DeviceID]) > 0 && !p.stepped[in.DeviceID] {
				reply(w, 403, map[string]string{"refusal": refusalStepUpRequired})
				return
			}
			if len(p.rows[in.DeviceID]) == 0 && p.windowClosed[in.DeviceID] && !p.stepped[in.DeviceID] {
				reply(w, 403, map[string]string{"refusal": refusalPairingWindowClosed})
				return
			}
			p.rows[in.DeviceID] = append(p.rows[in.DeviceID], storedPasskey{RowID: p.nextRow, Credential: in.Cred})
			p.nextRow++
			reply(w, 201, map[string]bool{"ok": true})
		case r.Method == "POST" && path == "/remote/step-up":
			var in struct {
				DeviceID int64 `json:"device_id"`
			}
			_ = json.NewDecoder(r.Body).Decode(&in)
			if !p.owners[in.DeviceID] || len(p.rows[in.DeviceID]) == 0 {
				reply(w, 403, map[string]string{"refusal": refusalNoPasskey})
				return
			}
			p.stepped[in.DeviceID] = true
			p.stepUps++
			reply(w, 200, map[string]any{"stepped_up": true, "window_seconds": 300})
		default:
			reply(w, 404, map[string]string{"refusal": "no_route"})
		}
	}
}

type passkeyRig struct {
	daemon *passkeyDaemon
	gw     func(method, path, body, session string) (int, string)
}

func newPasskeyRig(t *testing.T) *passkeyRig {
	t.Helper()
	pd := newPasskeyDaemon()
	srv := g2Daemon(t, pd.handler(t))
	gw := g2Gateway(t, g2Config(srv.URL))
	return &passkeyRig{daemon: pd, gw: func(method, path, body, session string) (int, string) {
		hdr := map[string]string{}
		if session != "" {
			hdr = g2Cookie(session)
		}
		resp := g2Do(t, gw, method, path, body, hdr)
		return resp.StatusCode, g2Body(t, resp)
	}}
}

const (
	sessA = "nos_device_AAAA"
	sessB = "nos_device_BBBB"
)

// enrol registers the authenticator for the session and fails the test if that is refused.
func (r *passkeyRig) enrol(t *testing.T, session string, auth *virtualAuthenticator) {
	t.Helper()
	status, opts := r.gw("POST", "/auth/passkey/register/begin", "", session)
	if status != 200 {
		t.Fatalf("register/begin = %d %s", status, opts)
	}
	status, body := r.gw("POST", "/auth/passkey/register/finish", auth.create(t, opts), session)
	if status != 200 {
		t.Fatalf("register/finish = %d %s", status, body)
	}
}

func (r *passkeyRig) verify(t *testing.T, session string, auth *virtualAuthenticator, bump bool) (int, string) {
	t.Helper()
	status, opts := r.gw("POST", "/auth/passkey/verify/begin", "", session)
	if status != 200 {
		return status, opts
	}
	return r.gw("POST", "/auth/passkey/verify/finish", auth.get(t, opts, bump), session)
}

func TestRegisterThenVerifyOpensStepUp(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	auth := newVirtualAuthenticator(testOrigin)

	rig.enrol(t, sessA, auth)
	if rig.daemon.stepped[7] {
		t.Fatal("registering must not open the step-up window")
	}
	status, body := rig.verify(t, sessA, auth, true)
	if status != 200 || !strings.Contains(body, `"stepped_up":true`) {
		t.Fatalf("verify = %d %s", status, body)
	}
	if !rig.daemon.stepped[7] {
		t.Fatal("the daemon was not asked to open the window")
	}
	// The sign count the authenticator reported is what the daemon now holds.
	if got := rig.daemon.rows[7][0].Credential; !strings.Contains(got, `"signCount":1`) {
		t.Fatalf("stored credential does not carry the new sign count: %s", got)
	}
}

func TestVerifyWithAnotherDevicesCredentialFails(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	rig.daemon.addDevice(sessB, 8, true)
	a, b := newVirtualAuthenticator(testOrigin), newVirtualAuthenticator(testOrigin)
	rig.enrol(t, sessA, a)
	rig.enrol(t, sessB, b)

	// Device 8 answers its own challenge with device 7's passkey.
	status, opts := rig.gw("POST", "/auth/passkey/verify/begin", "", sessB)
	if status != 200 {
		t.Fatalf("begin = %d", status)
	}
	status, body := rig.gw("POST", "/auth/passkey/verify/finish", a.get(t, opts, true), sessB)
	if status == 200 || rig.daemon.stepped[8] {
		t.Fatalf("another device's credential opened a step-up: %d %s", status, body)
	}
}

func TestVerifyWithWrongOriginFails(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	auth := newVirtualAuthenticator(testOrigin)
	rig.enrol(t, sessA, auth)

	auth.origin = "https://evil.example"
	status, body := rig.verify(t, sessA, auth, true)
	if status != 400 || rig.daemon.stepped[7] {
		t.Fatalf("a wrong origin was accepted: %d %s", status, body)
	}
}

func TestRegisterWithWrongOriginFails(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	auth := newVirtualAuthenticator("https://evil.example")
	status, opts := rig.gw("POST", "/auth/passkey/register/begin", "", sessA)
	if status != 200 {
		t.Fatalf("begin = %d", status)
	}
	status, _ = rig.gw("POST", "/auth/passkey/register/finish", auth.create(t, opts), sessA)
	if status != 400 || len(rig.daemon.rows[7]) != 0 {
		t.Fatalf("status = %d, rows = %d", status, len(rig.daemon.rows[7]))
	}
}

func TestVerifyIgnoresTheRequestsHostAndOrigin(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	auth := newVirtualAuthenticator(testOrigin)
	rig.enrol(t, sessA, auth)
	_, opts := rig.gw("POST", "/auth/passkey/verify/begin", "", sessA)
	if !strings.Contains(opts, `"rpId":"box.tail1234.ts.net"`) {
		t.Fatalf("the relying party id is not the configured host: %s", opts)
	}
}

func TestReplayingAFinishedCeremonyFails(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	auth := newVirtualAuthenticator(testOrigin)
	rig.enrol(t, sessA, auth)

	status, opts := rig.gw("POST", "/auth/passkey/verify/begin", "", sessA)
	if status != 200 {
		t.Fatal(status)
	}
	answer := auth.get(t, opts, true)
	if status, body := rig.gw("POST", "/auth/passkey/verify/finish", answer, sessA); status != 200 {
		t.Fatalf("first finish = %d %s", status, body)
	}
	rig.daemon.stepped[7] = false
	status, body := rig.gw("POST", "/auth/passkey/verify/finish", answer, sessA)
	if status != 400 || !strings.Contains(body, refusalCeremony) || rig.daemon.stepped[7] {
		t.Fatalf("a replay was accepted: %d %s", status, body)
	}
}

func TestACeremonyBelongsToTheSessionThatBeganIt(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	rig.daemon.addDevice(sessB, 7, true) // same device row seen through another cookie
	auth := newVirtualAuthenticator(testOrigin)
	rig.enrol(t, sessA, auth)

	_, opts := rig.gw("POST", "/auth/passkey/verify/begin", "", sessA)
	status, _ := rig.gw("POST", "/auth/passkey/verify/finish", auth.get(t, opts, true), sessB)
	if status != 400 || rig.daemon.stepped[7] {
		t.Fatalf("another session finished the ceremony: %d", status)
	}
}

func TestACeremonyExpires(t *testing.T) {
	d := newPasskeyDaemon()
	d.addDevice(sessA, 7, true)
	srv := g2Daemon(t, d.handler(t))
	h := NewAuth(g2Config(srv.URL)).(*authHandler)
	now := time.Now()
	h.cer.now = func() time.Time { return now }

	h.cer.put(ceremonyKey("verify", sessA), ceremony{deviceID: 7})
	now = now.Add(ceremonyTTL + time.Second)
	if _, ok := h.cer.take(ceremonyKey("verify", sessA)); ok {
		t.Fatal("an expired ceremony was returned")
	}
	h.cer.put(ceremonyKey("verify", sessA), ceremony{deviceID: 7})
	if _, ok := h.cer.take(ceremonyKey("verify", sessA)); !ok {
		t.Fatal("a fresh ceremony was lost")
	}
	if _, ok := h.cer.take(ceremonyKey("verify", sessA)); ok {
		t.Fatal("a ceremony was used twice")
	}
}

func TestAClonedAuthenticatorIsRefused(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	auth := newVirtualAuthenticator(testOrigin)
	rig.enrol(t, sessA, auth)
	if status, body := rig.verify(t, sessA, auth, true); status != 200 {
		t.Fatalf("first verify = %d %s", status, body)
	}
	rig.daemon.stepped[7] = false
	// The same counter again: a copy of the key that has not seen the stored count.
	status, body := rig.verify(t, sessA, auth, false)
	if status != 400 || rig.daemon.stepped[7] {
		t.Fatalf("a clone warning did not fail the verify: %d %s", status, body)
	}
}

func TestOnlyAnOwnerRegisters(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, false)
	for _, path := range []string{"/auth/passkey/register/begin", "/auth/passkey/verify/begin"} {
		if status, body := rig.gw("POST", path, "", sessA); status != 403 || !strings.Contains(body, refusalNotOwner) {
			t.Fatalf("%s = %d %s", path, status, body)
		}
	}
}

func TestRegisterBeginAfterWindowIsRefused(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	rig.daemon.windowClosed[7] = true

	status, body := rig.gw("POST", "/auth/passkey/register/begin", "", sessA)
	if status != 403 || !strings.Contains(body, refusalPairingWindowClosed) {
		t.Fatalf("register/begin after the window = %d %s", status, body)
	}
	if status, body := rig.gw("GET", "/auth/passkey/status", "", sessA); status != 200 || !strings.Contains(body, `"can_register":false`) {
		t.Fatalf("status after the window = %d %s", status, body)
	}
}

func TestASecondPasskeyNeedsAStepUp(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	first := newVirtualAuthenticator(testOrigin)
	rig.enrol(t, sessA, first)

	// A session that stole the owner cookie but cannot pass the ceremony is not stepped up.
	status, body := rig.gw("POST", "/auth/passkey/register/begin", "", sessA)
	if status != 403 || !strings.Contains(body, refusalStepUpRequired) {
		t.Fatalf("second register/begin = %d %s", status, body)
	}
	if status, body := rig.verify(t, sessA, first, true); status != 200 {
		t.Fatalf("verify = %d %s", status, body)
	}
	rig.enrol(t, sessA, newVirtualAuthenticator(testOrigin))
	if len(rig.daemon.rows[7]) != 2 {
		t.Fatalf("rows = %d, want 2", len(rig.daemon.rows[7]))
	}
}

func TestVerifyWithNoPasskeyIsConflict(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	if status, body := rig.gw("POST", "/auth/passkey/verify/begin", "", sessA); status != 409 || !strings.Contains(body, refusalNoPasskey) {
		t.Fatalf("= %d %s", status, body)
	}
}

func TestPasskeyRoutesNeedASession(t *testing.T) {
	rig := newPasskeyRig(t)
	for _, path := range []string{"/auth/passkey/register/begin", "/auth/passkey/verify/finish"} {
		if status, _ := rig.gw("POST", path, "{}", ""); status != 401 {
			t.Fatalf("%s without a cookie = %d, want 401", path, status)
		}
	}
	if status, _ := rig.gw("GET", "/auth/passkey/register/begin", "", sessA); status != 405 {
		t.Fatalf("GET on a POST route = %d", status)
	}
}

func TestPasskeyStatus(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	status, body := rig.gw("GET", "/auth/passkey/status", "", sessA)
	if status != 200 || !strings.Contains(body, `"registered":false`) || !strings.Contains(body, `"can_register":true`) {
		t.Fatalf("= %d %s", status, body)
	}
}

func TestPasskeyNeverLogsOrEchoesCredentials(t *testing.T) {
	rig := newPasskeyRig(t)
	rig.daemon.addDevice(sessA, 7, true)
	auth := newVirtualAuthenticator(testOrigin)
	rig.enrol(t, sessA, auth)
	_, body := rig.verify(t, sessA, auth, true)
	if strings.Contains(body, "publicKey") || strings.Contains(body, sessA) {
		t.Fatalf("the finish answer carries credential or session material: %s", body)
	}
}

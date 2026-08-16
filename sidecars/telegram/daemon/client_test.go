package daemon

import (
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestClientSendAssistantMessageAndGetKill(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if got := r.Header.Get("Authorization"); got != "Bearer tok" {
			t.Errorf("Authorization = %q, want %q", got, "Bearer tok")
		}

		switch r.URL.RequestURI() {
		case "/assistant/message":
			if r.Method != http.MethodPost {
				t.Errorf("method = %s, want POST", r.Method)
			}
			if got := r.Header.Get("Content-Type"); got != "application/json" {
				t.Errorf("Content-Type = %q, want application/json", got)
			}
			var request map[string]string
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Errorf("decode request: %v", err)
			}
			if request["chat_id"] != "chat-9" || request["text"] != "hello" {
				t.Errorf("request = %#v, want chat_id and text", request)
			}
			// The daemon routes the turn on this. Dropped, every Telegram message would go
			// back to the cloud CLI and nothing would look broken — which is why it is
			// asserted here rather than left to be noticed.
			if request["origin"] != "telegram" {
				t.Errorf("origin = %q, want %q", request["origin"], "telegram")
			}
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"turn_id":81}`))
		case "/autopilot/kill":
			if r.Method != http.MethodGet {
				t.Errorf("method = %s, want GET", r.Method)
			}
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"engaged":true}`))
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	client := New(server.URL, "tok")

	turnID, err := client.SendAssistantMessage("chat-9", "hello")
	if err != nil {
		t.Fatalf("SendAssistantMessage() error = %v", err)
	}
	if turnID != 81 {
		t.Errorf("turnID = %d, want 81", turnID)
	}

	engaged, err := client.GetKill()
	if err != nil {
		t.Fatalf("GetKill() error = %v", err)
	}
	if !engaged {
		t.Error("GetKill() = false, want true")
	}
}

func TestClientGetProjects(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/projects" {
			t.Errorf("path = %q, want %q", r.URL.Path, "/projects")
		}
		if r.Method != http.MethodGet {
			t.Errorf("method = %s, want GET", r.Method)
		}
		if got := r.Header.Get("Authorization"); got != "Bearer tok" {
			t.Errorf("Authorization = %q, want %q", got, "Bearer tok")
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`[{"project_id":"a","mode":"off","project_root":null,"pending":2},{"project_id":"b","mode":"shadow","project_root":"C:\\x","pending":0}]`))
	}))
	defer server.Close()

	projects, err := New(server.URL, "tok").GetProjects()
	if err != nil {
		t.Fatalf("GetProjects() error = %v", err)
	}
	if len(projects) != 2 {
		t.Fatalf("len(GetProjects()) = %d, want 2", len(projects))
	}
	if got := projects[0]["project_id"]; got != "a" {
		t.Errorf("first project_id = %v, want a", got)
	}
	if got := projects[1]["project_id"]; got != "b" {
		t.Errorf("second project_id = %v, want b", got)
	}
}

// The errand routes, and the one thing about them that is not obvious: `/pausa` typed in a topic
// has to find the errand of THAT topic, and the only handle the sidecar holds is the chat key. So
// the lookup is a list filtered by key, and a topic with no errand is a normal answer rather than
// an error — most topics do not have one.
func TestClientErrandRoutes(t *testing.T) {
	var patched map[string]any
	var created map[string]string
	var closedPath string

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch {
		case r.URL.Path == "/errands" && r.Method == http.MethodGet:
			_, _ = w.Write([]byte(`[
				{"id":4,"name":"carros","chat_key":"-100123:7","brain":"local","folder":"carros-4","status":"active"},
				{"id":5,"name":"casa","chat_key":"-100123:9","brain":"cloud","folder":"casa-5","status":"paused"}
			]`))
		case r.URL.Path == "/errands" && r.Method == http.MethodPost:
			_ = json.NewDecoder(r.Body).Decode(&created)
			_, _ = w.Write([]byte(`{"errand_id":6}`))
		case r.URL.Path == "/errands/4" && r.Method == http.MethodPatch:
			_ = json.NewDecoder(r.Body).Decode(&patched)
			w.WriteHeader(http.StatusNoContent)
		case r.URL.Path == "/errands/4" && r.Method == http.MethodDelete:
			closedPath = r.URL.Path
			w.WriteHeader(http.StatusNoContent)
		default:
			t.Errorf("unexpected %s %s", r.Method, r.URL.Path)
			w.WriteHeader(http.StatusNotFound)
		}
	}))
	defer server.Close()

	client := New(server.URL, "tok")

	errands, err := client.ListErrands()
	if err != nil {
		t.Fatalf("ListErrands: %v", err)
	}
	if len(errands) != 2 || errands[0].Name != "carros" || errands[0].ChatKey != "-100123:7" {
		t.Fatalf("ListErrands = %+v", errands)
	}

	found, ok, err := client.ErrandOfChat("-100123:9")
	if err != nil || !ok {
		t.Fatalf("ErrandOfChat: %+v, %v, %v", found, ok, err)
	}
	if found.ID != 5 || found.Status != "paused" || found.Brain != "cloud" {
		t.Fatalf("ErrandOfChat = %+v", found)
	}

	// A topic with no errand is the common case, and it is an answer and not a failure: almost no
	// topic has one, and reporting it as an error would put "couldn't reach the daemon" in front of
	// somebody who simply typed `/pausa` in the wrong place.
	if _, ok, err := client.ErrandOfChat("-100123:404"); err != nil || ok {
		t.Fatalf("a topic with no errand should answer not-found: ok=%v err=%v", ok, err)
	}

	id, err := client.CreateErrand("barcos", "-100123:11")
	if err != nil {
		t.Fatalf("CreateErrand: %v", err)
	}
	if id != 6 {
		t.Errorf("CreateErrand id = %d, want 6", id)
	}
	if created["name"] != "barcos" || created["chat_key"] != "-100123:11" {
		t.Errorf("CreateErrand body = %#v", created)
	}

	if err := client.SetErrandStatus(4, "paused"); err != nil {
		t.Fatalf("SetErrandStatus: %v", err)
	}
	if patched["status"] != "paused" {
		t.Errorf("patch body = %#v, want status", patched)
	}
	// The two PATCH fields are sent one at a time on purpose: `/pausa` must not also restate the
	// brain, or a command about one thing quietly rewrites another.
	if _, present := patched["brain"]; present {
		t.Errorf("a status change must not carry a brain: %#v", patched)
	}

	patched = nil
	if err := client.SetErrandBrain(4, "cloud"); err != nil {
		t.Fatalf("SetErrandBrain: %v", err)
	}
	if patched["brain"] != "cloud" {
		t.Errorf("patch body = %#v, want brain", patched)
	}
	if _, present := patched["status"]; present {
		t.Errorf("a brain change must not carry a status: %#v", patched)
	}

	if err := client.CloseErrand(4); err != nil {
		t.Fatalf("CloseErrand: %v", err)
	}
	if closedPath != "/errands/4" {
		t.Errorf("CloseErrand path = %q", closedPath)
	}
}

// A refusal arrives as data, not as a sentence. The núcleo names what it refused in the body, and
// four of its refusals share three status codes — so a client that keeps only the number cannot
// tell a paused errand (which clears when somebody resumes it) from a chat mid-turn (which clears
// on its own). Both are 409.
func TestARefusalCarriesTheNucleosOwnNameForIt(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusConflict)
		_, _ = w.Write([]byte(`{"refusal":"errand_not_answering"}`))
	}))
	defer server.Close()

	_, err := New(server.URL, "tok").SendAssistantMessage("-100123:7", "procura")

	var refused *StatusError
	if !errors.As(err, &refused) {
		t.Fatalf("error = %v (%T), want a *StatusError", err, err)
	}
	if refused.Status != http.StatusConflict {
		t.Errorf("Status = %d, want 409", refused.Status)
	}
	if refused.Refusal != "errand_not_answering" {
		t.Errorf("Refusal = %q, want the name the núcleo gave it", refused.Refusal)
	}
}

// An older núcleo, or any refusal on a route that does not name them, still has to arrive as a
// refusal. An empty name is an answer here — the caller falls back to saying what it can — and
// never a parse failure that turns a stated refusal into a broken client.
func TestARefusalWithNoNameIsStillARefusal(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = w.Write([]byte("something went wrong"))
	}))
	defer server.Close()

	_, err := New(server.URL, "tok").SendAssistantMessage("chat-9", "olá")

	var refused *StatusError
	if !errors.As(err, &refused) {
		t.Fatalf("error = %v (%T), want a *StatusError", err, err)
	}
	if refused.Refusal != "" {
		t.Errorf("Refusal = %q, want empty for a body that names none", refused.Refusal)
	}
	if !strings.Contains(refused.Error(), "something went wrong") {
		t.Errorf("Error() = %q, want the body it could not name", refused.Error())
	}
}

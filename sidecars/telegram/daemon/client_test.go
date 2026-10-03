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

// A refusal arrives as data, not as a sentence. The núcleo names what it refused in the body, and
// several of its refusals share a status code — so a client that keeps only the number cannot
// tell a chat mid-turn (which clears on its own) from a missing local model (which does not).
// Both are 409.
func TestARefusalCarriesTheNucleosOwnNameForIt(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusConflict)
		_, _ = w.Write([]byte(`{"refusal":"no_local_model"}`))
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
	if refused.Refusal != "no_local_model" {
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

func TestCreateNotePostsTextWithTelegramOrigin(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			t.Errorf("method = %s, want POST", r.Method)
		}
		if r.URL.Path != "/owner-notes" {
			t.Errorf("path = %q, want /owner-notes", r.URL.Path)
		}
		var request map[string]string
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Errorf("decode request: %v", err)
		}
		if request["text"] != "remember the milk" {
			t.Errorf("text = %q, want %q", request["text"], "remember the milk")
		}
		if request["origin"] != "telegram" {
			t.Errorf("origin = %q, want telegram", request["origin"])
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusCreated)
		_, _ = w.Write([]byte(`{"id":17}`))
	}))
	defer server.Close()

	id, err := New(server.URL, "tok").CreateNote("remember the milk")
	if err != nil {
		t.Fatalf("CreateNote() error = %v", err)
	}
	if id != 17 {
		t.Errorf("id = %d, want 17", id)
	}
}

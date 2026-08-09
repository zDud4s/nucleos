package daemon

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
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

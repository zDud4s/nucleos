// Package daemon is the typed HTTP client for the two núcleo routes this sidecar uses.
//
// The sidecar never writes to the mailbox and never writes to the database. Its whole output is
// one POST, and the cursor it reads back is the núcleo's, not its own — so a delivery that fails
// simply repeats, and nothing has to be reconciled between two processes.
package daemon

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"time"
)

type Client struct {
	baseURL string
	token   string
	http    *http.Client
}

func New(baseURL, token string) *Client {
	return &Client{
		baseURL: baseURL,
		token:   token,
		http:    &http.Client{Timeout: 60 * time.Second},
	}
}

// Cursor is the stored position in a mailbox. A nil cursor means the mailbox was never
// synchronised, which the núcleo treats as one of the conditions that arm the backfill cutoff.
type Cursor struct {
	UIDValidity uint32 `json:"uidvalidity"`
	LastUID     uint32 `json:"last_uid"`
}

// Attachment is what a message carries besides its text — described, not delivered.
//
// The BYTES are deliberately absent. The núcleo keeps what a person needs in order to decide
// whether something is worth opening (a name, a type, a size) and asks for the content only when
// someone actually asks, so a stranger's executable never lands on disk unrequested.
type Attachment struct {
	// Which attachment, counting from zero in the order the message carries them — NOT a MIME part
	// number. It is how the content is requested later, and it is stable because it is re-derived
	// by walking the same message the same way.
	Position  int    `json:"position"`
	Filename  string `json:"filename,omitempty"`
	MimeType  string `json:"mime_type,omitempty"`
	SizeBytes int64  `json:"size_bytes"`
}

// Message is one delivered message, in the envelope the núcleo's `POST /email/incoming` expects.
type Message struct {
	MessageID      string            `json:"message_id,omitempty"`
	UID            uint32            `json:"uid"`
	FromAddr       string            `json:"from_addr"`
	FromName       string            `json:"from_name,omitempty"`
	Subject        string            `json:"subject,omitempty"`
	ReceivedAt     string            `json:"received_at"`
	BodyText       string            `json:"body_text,omitempty"`
	HasAttachments bool              `json:"has_attachments"`
	Attachments    []Attachment      `json:"attachments,omitempty"`
	Headers        map[string]string `json:"headers,omitempty"`
}

// Skipped is a message that was looked at and could not be read. It carries the uid so the cursor
// can move past it, and the reason so the user is told rather than quietly losing mail.
type Skipped struct {
	UID    uint32 `json:"uid"`
	Reason string `json:"reason"`
}

type Batch struct {
	Mailbox   string `json:"mailbox"`
	Direction string `json:"direction"`
	// UIDValidity identifies the server's uid space; a change means the old position is not a
	// position any more.
	UIDValidity uint32 `json:"uidvalidity"`
	// MaxUIDExamined is the highest uid whose fetch COMPLETED and got a decision — never the
	// highest the search returned. A connection that dies mid-fetch would otherwise advance the
	// cursor over mail nobody ever read.
	MaxUIDExamined uint32    `json:"max_uid_examined"`
	Skipped        []Skipped `json:"skipped"`
	Messages       []Message `json:"messages"`
}

type IngestResult struct {
	Ingested   int    `json:"ingested"`
	Duplicates int    `json:"duplicates"`
	Cursor     uint32 `json:"cursor"`
}

func (c *Client) do(req *http.Request) ([]byte, error) {
	req.Header.Set("Authorization", "Bearer "+c.token)
	resp, err := c.http.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("daemon returned %d: %s", resp.StatusCode, string(body))
	}
	return body, nil
}

// GetCursor reads the stored position. A JSON `null` means there is none.
func (c *Client) GetCursor(mailbox string) (*Cursor, error) {
	endpoint := fmt.Sprintf("%s/email/cursor?mailbox=%s", c.baseURL, url.QueryEscape(mailbox))
	req, err := http.NewRequest(http.MethodGet, endpoint, nil)
	if err != nil {
		return nil, err
	}
	body, err := c.do(req)
	if err != nil {
		return nil, err
	}
	var cursor *Cursor
	if err := json.Unmarshal(body, &cursor); err != nil {
		return nil, fmt.Errorf("could not read the cursor: %w", err)
	}
	return cursor, nil
}

// Deliver hands one batch to the núcleo, which stores it and moves the cursor in one transaction.
func (c *Client) Deliver(batch Batch) (IngestResult, error) {
	// A nil slice marshals to `null`, and a field this side declares as a list must arrive as one.
	// The núcleo tolerates `null` too, but sending it was the bug: an inbox with nothing skipped —
	// the ordinary case — produced a batch the daemon rejected, and it replayed every poll.
	if batch.Skipped == nil {
		batch.Skipped = []Skipped{}
	}
	if batch.Messages == nil {
		batch.Messages = []Message{}
	}
	payload, err := json.Marshal(batch)
	if err != nil {
		return IngestResult{}, err
	}
	req, err := http.NewRequest(
		http.MethodPost,
		c.baseURL+"/email/incoming",
		bytes.NewReader(payload),
	)
	if err != nil {
		return IngestResult{}, err
	}
	req.Header.Set("Content-Type", "application/json")

	body, err := c.do(req)
	if err != nil {
		return IngestResult{}, err
	}
	var result IngestResult
	if err := json.Unmarshal(body, &result); err != nil {
		return IngestResult{}, fmt.Errorf("could not read the ingest result: %w", err)
	}
	return result, nil
}

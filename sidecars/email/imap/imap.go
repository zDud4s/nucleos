// Package imap is the read-only mailbox reader.
//
// THE INVARIANT (spec §3.1): this package never modifies the mailbox. No STORE, no MOVE, no COPY,
// no EXPUNGE, and no flag is ever set — the mailbox is opened with EXAMINE (read-only select) and
// bodies are read with BODY.PEEK, which is what stops a fetch from marking mail as read. The user
// calibrating this pillar must find their inbox exactly as they left it.
//
// A test in this package asserts the invariant against the source itself, so adding a mutating
// call fails the build rather than a mailbox.
package imap

import (
	"fmt"
	"time"

	imapv2 "github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapclient"
)

// Raw is one fetched message before extraction: the bytes, plus the two facts only the server can
// tell us.
type Raw struct {
	UID          uint32
	InternalDate time.Time
	Body         []byte
}

type Conn struct {
	client *imapclient.Client
}

// Dial opens a TLS connection and authenticates.
func Dial(addr, username, password string) (*Conn, error) {
	client, err := imapclient.DialTLS(addr, nil)
	if err != nil {
		return nil, fmt.Errorf("could not connect to %s: %w", addr, err)
	}
	if err := client.Login(username, password).Wait(); err != nil {
		client.Close()
		return nil, fmt.Errorf("could not authenticate as %s: %w", username, err)
	}
	return &Conn{client: client}, nil
}

func (c *Conn) Close() {
	if c.client != nil {
		_ = c.client.Logout().Wait()
		_ = c.client.Close()
	}
}

// Select opens the mailbox READ-ONLY and returns its UIDVALIDITY.
//
// Read-only is not a preference: a writable SELECT is what lets a later fetch set \Seen, and the
// whole pillar is built on not touching the user's mail.
func (c *Conn) Select(mailbox string) (uint32, error) {
	data, err := c.client.Select(mailbox, &imapv2.SelectOptions{ReadOnly: true}).Wait()
	if err != nil {
		return 0, fmt.Errorf("could not select %s: %w", mailbox, err)
	}
	return data.UIDValidity, nil
}

// SearchAbove returns the uids strictly greater than `lastUID`, oldest first.
func (c *Conn) SearchAbove(lastUID uint32) ([]imapv2.UID, error) {
	criteria := &imapv2.SearchCriteria{
		UID: []imapv2.UIDSet{imapv2.UIDSetNum()},
	}
	// `lastUID+1:*` is the whole tail. An empty mailbox simply returns nothing.
	criteria.UID[0] = imapv2.UIDSet{imapv2.UIDRange{
		Start: imapv2.UID(lastUID + 1),
		Stop:  0, // 0 is `*` — the highest uid present.
	}}

	data, err := c.client.UIDSearch(criteria, nil).Wait()
	if err != nil {
		return nil, fmt.Errorf("could not search above uid %d: %w", lastUID, err)
	}
	return data.AllUIDs(), nil
}

// SearchSince returns the uids of messages the server received since `since`, for the case where
// UIDVALIDITY changed and the stored position means nothing any more.
func (c *Conn) SearchSince(since time.Time) ([]imapv2.UID, error) {
	criteria := &imapv2.SearchCriteria{Since: since}
	data, err := c.client.UIDSearch(criteria, nil).Wait()
	if err != nil {
		return nil, fmt.Errorf("could not search since %s: %w", since.Format(time.RFC3339), err)
	}
	return data.AllUIDs(), nil
}

// Fetch reads one message whole, WITHOUT marking it read.
//
// One at a time on purpose: the caller advances its watermark per completed message, so a
// connection that dies mid-run reports what it actually read rather than what it hoped to.
func (c *Conn) Fetch(uid imapv2.UID) (Raw, error) {
	set := imapv2.UIDSetNum(uid)
	options := &imapv2.FetchOptions{
		UID:          true,
		InternalDate: true,
		// Peek is the flag that keeps this read-only: without it the server sets \Seen.
		BodySection: []*imapv2.FetchItemBodySection{{Peek: true}},
	}

	messages, err := c.client.Fetch(set, options).Collect()
	if err != nil {
		return Raw{}, fmt.Errorf("could not fetch uid %d: %w", uid, err)
	}
	if len(messages) == 0 {
		return Raw{}, fmt.Errorf("uid %d returned no message", uid)
	}

	message := messages[0]
	var body []byte
	for _, section := range message.BodySection {
		body = section.Bytes
		break
	}
	if body == nil {
		return Raw{}, fmt.Errorf("uid %d returned no body", uid)
	}

	return Raw{
		UID:          uint32(message.UID),
		InternalDate: message.InternalDate,
		Body:         body,
	}, nil
}

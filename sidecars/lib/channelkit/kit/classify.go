package kit

import (
	"encoding/json"
	"net/http"
)

// Endpoint names a contract route, because the same status means different
// things on different routes (a 400 skips an event but gives up an ack).
type Endpoint int

const (
	EndpointHello Endpoint = iota
	EndpointEvents
	EndpointOutbox
	EndpointAck
	EndpointRenew
	EndpointMedia
)

// Class is what an adapter must do with a response.
type Class int

const (
	// OK: the request succeeded.
	OK Class = iota
	// Retry: transient (network error, 408, 429, 5xx); try again with backoff.
	Retry
	// SkipEvent: the core permanently rejected this event; advance the cursor and count the failure.
	SkipEvent
	// SendAsUnsupported: the media is over the limit; degrade to an unsupported part.
	SendAsUnsupported
	// StopRenewing: the action is no longer leased; stop renewing it.
	StopRenewing
	// GiveUp: an ack or renewal was rejected; do not repeat it.
	GiveUp
	// NotFound: the referenced resource does not exist.
	NotFound
	// Fatal: misconfiguration or incompatibility; the adapter must terminate.
	Fatal
)

func (c Class) String() string {
	names := [...]string{"OK", "Retry", "SkipEvent", "SendAsUnsupported", "StopRenewing", "GiveUp", "NotFound", "Fatal"}
	if c < 0 || int(c) >= len(names) {
		return "Class(?)"
	}
	return names[c]
}

// errorCode extracts the `error` field of a contract error body, or "".
func errorCode(body []byte) string {
	var e struct {
		Error string `json:"error"`
	}
	if json.Unmarshal(body, &e) != nil {
		return ""
	}
	return e.Error
}

// Classify maps a response to the action the adapter takes. Status 0 stands for
// a network error (no response at all). The body is consulted only to tell the
// 409 variants apart, by its `error` field.
func Classify(endpoint Endpoint, status int, body []byte) Class {
	switch {
	case status >= 200 && status < 300:
		return OK
	case status == 0, status == http.StatusRequestTimeout, status == http.StatusTooManyRequests, status >= 500:
		return Retry
	case status == http.StatusUnauthorized:
		return Fatal
	}

	code := errorCode(body)
	switch status {
	case http.StatusConflict:
		if code == "not_leased" && (endpoint == EndpointAck || endpoint == EndpointRenew) {
			return StopRenewing
		}
		return Fatal // contract_version, or anything unexpected
	case http.StatusForbidden:
		return Fatal // channel_mismatch or a bad token
	}

	switch endpoint {
	case EndpointEvents:
		switch status {
		case http.StatusBadRequest, http.StatusRequestEntityTooLarge, http.StatusUnprocessableEntity:
			return SkipEvent
		}
	case EndpointAck, EndpointRenew:
		switch status {
		case http.StatusBadRequest, http.StatusNotFound:
			return GiveUp
		}
	case EndpointMedia:
		switch status {
		case http.StatusRequestEntityTooLarge:
			return SendAsUnsupported
		case http.StatusNotFound:
			return NotFound
		}
	}
	return Fatal
}

package contract

// eventResponseRaw is the wire form of the core's answer to an event. The
// pointers tell an absent flag from an explicit false.
type eventResponseRaw struct {
	Accepted  *bool `json:"accepted,omitempty"`
	Duplicate *bool `json:"duplicate,omitempty"`
	Dropped   *bool `json:"dropped,omitempty"`
}

// Validate requires exactly one flag, and that flag must be true.
func (r *eventResponseRaw) Validate() *Error {
	flags := []struct {
		name string
		set  *bool
	}{{"accepted", r.Accepted}, {"duplicate", r.Duplicate}, {"dropped", r.Dropped}}
	n, at := 0, ""
	for _, f := range flags {
		if f.set != nil {
			n++
			if !*f.set {
				at = "/" + f.name
			}
		}
	}
	if n != 1 {
		return errOneOf("")
	}
	if at != "" {
		return errValue(at)
	}
	return nil
}

// EventResponse is the core's answer to a delivered event (an output shape):
// exactly one flag is set.
type EventResponse struct {
	Accepted  bool `json:"accepted,omitempty"`
	Duplicate bool `json:"duplicate,omitempty"`
	Dropped   bool `json:"dropped,omitempty"`
}

// Advance reports whether the adapter may move its platform cursor past the
// event: every defined answer (accepted, duplicate, dropped) means the core is
// done with it.
func (r EventResponse) Advance() bool { return r.Accepted || r.Duplicate || r.Dropped }

// DecodeEventResponse decodes an event response leniently (an output shape).
func DecodeEventResponse(b []byte) (EventResponse, error) {
	r, err := DecodeLenient[eventResponseRaw](b)
	if err != nil {
		return EventResponse{}, err
	}
	return EventResponse{
		Accepted:  r.Accepted != nil,
		Duplicate: r.Duplicate != nil,
		Dropped:   r.Dropped != nil,
	}, nil
}

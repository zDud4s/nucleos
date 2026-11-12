Go review patterns: what to look for in .go changes (sidecars: echo, email, telegram, web).

Errors
- `_ = f()`, unchecked `Close()`, `Write()`, `Flush()`, `Scan()`. Tell: a bare call statement returning error.
- `err` shadowed by `:=` in an inner block (`if x, err := ...`, `for`), so the outer err stays nil and is returned.
- Returning `nil` after a failed step; `%v` instead of `%w` losing the chain.

defer and loops
- `defer` inside a loop (resources held until function return).
- Loop variable captured by a closure or goroutine: shared per loop before Go 1.22. Check `go` directive in go.mod; `tt := tt` was the old fix.
- `defer f.Close()` before the err check of the open; deferred args evaluated at defer time.

Goroutines and channels
- Goroutine with no exit path: blocked on unbuffered send with no receiver, `for { select }` with no `ctx.Done()`, missing `cancel()` after `WithCancel/WithTimeout`.
- `wg.Add(1)` inside the goroutine (race with Wait); `wg.Done` not deferred.
- Channel closed by a receiver, closed twice, or sent on after close (panic).
- `context` not passed down to HTTP/exec/DB calls, or stored in a struct field.
- Data race: map or slice or struct fields touched from several goroutines without a mutex; `sync.Mutex` copied by value. Cannot run it here: name `go test -race ./...` for the controller.

Nil and slices
- Write to a nil map (panic); read is fine.
- Interface holding a typed nil: `var p *T = nil; return p` as `error`/interface is `!= nil`.
- `append` aliasing: two slices sharing a backing array, mutated after append to a sub-slice.

HTTP, IO, exec
- `http.Client{}` or `http.Get` with no Timeout/ctx deadline; `resp.Body` not closed (also on non-2xx); status code unchecked.
- `io.ReadAll` on untrusted or unbounded input without `io.LimitReader`.
- `exec.Command("sh", "-c", fmt.Sprintf(...))` with external text; args built by string concatenation.

JSON and time
- Unexported struct fields silently skipped; `omitempty` dropping meaningful zero values (0, false, ""); missing/changed tags rename the wire key.
- Unmarshal into `interface{}`: numbers become float64 (large ids lose precision); unknown fields ignored unless `DisallowUnknownFields`.
- Time: comparing `time.Time` with `==` (monotonic, zone); storing local time; `time.After` in loops leaks timers before Go 1.23 (check the go directive); durations from untrusted ints overflowing.

Protocol with the daemon
- Sidecars speak to the Rust daemon. When a message struct, JSON tag, command name, or exit/ack semantic changes, Grep the Rust side (`core/`) and the other sidecars for the same key and confirm both sides changed together, including optional vs required fields.

Tests
- Table tests: `t.Run(tt.name, ...)` missing, or `t.Parallel()` inside using the captured loop var (pre-1.22) or shared state/ports/temp files.
- Sleep-based synchronization; tests that never assert; `t.Fatal` called from a non-test goroutine.

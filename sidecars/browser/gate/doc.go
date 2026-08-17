// Package gate is spec §11's first eight tests against a real browser.
//
// It is empty by design outside the `browsergate` build tag: the rest of the suite runs without
// Chrome in about ten seconds, and this group launches a browser per test. Run it with
//
//	go test -tags browsergate ./gate/
//
// # Why these eight and not more
//
// Everything the fence DECIDES is tested in the fence package, pure, with a table. What cannot be
// tested there is whether Chrome actually does what CDP is documented to do — and the spike of
// 2026-08-15 measured three documented mechanisms not working at all. This group exists for that
// gap and nothing else, which is why it is small and why every test here drives a real browser.
//
// # What this group does NOT cover, named rather than implied
//
// The https site allowlist of spec §5.4 is exercised here only through its loopback sibling. A real
// test needs two DIFFERENT hosts over https, and §11 forbids testing against the live internet — a
// suite that depends on somebody else's site fails for reasons that are not ours. Chrome's
// --host-resolver-rules would map two names onto loopback, but with --proxy-server set Chrome does
// not resolve at all: the proxy does, in Go, and giving it a swappable resolver would be a
// production hole opened for a test. So the wiring is what is proven here — that a refused document
// really does stop in Chrome — and WHICH rule refused it is proven in the fence package.
package gate

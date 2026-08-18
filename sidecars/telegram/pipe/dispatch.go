package pipe

import (
	"log"
	"runtime/debug"
	"sync"
	"time"
)

// queueDepth bounds how many updates can be waiting on one conversation. Reaching it means that
// conversation's worker has been stuck for a long time; the bound is what keeps a wedged worker
// from turning into unbounded memory.
const queueDepth = 64

// restartDelay keeps a loop that panics on every attempt from spinning.
const restartDelay = 5 * time.Second

// Dispatcher runs updates off the polling loop. Handling used to happen inside the loop, so one
// slow update held up every update behind it — including `/kill`, which is the one message this
// process exists to deliver. Work is serialised per CONVERSATION, because the núcleo hands each one
// turn at a time (its ChatSlot), but conversations do not block each other and none of them blocks
// the poller.
//
// The key is `ChatKey`'s and not a chat id, which is what makes two topics of one group two
// conversations here. Keyed on the chat, a group's topics would queue behind each other for no
// reason the núcleo has — it holds a slot per chat KEY — and the whole point of an errand per topic
// is that two of them can be working at once.
type Dispatcher struct {
	mu     sync.Mutex
	queues map[string]chan func()
	wg     sync.WaitGroup
	closed bool
}

func NewDispatcher() *Dispatcher {
	return &Dispatcher{queues: map[string]chan func(){}}
}

// Dispatch queues work for one conversation and returns immediately.
func (d *Dispatcher) Dispatch(key string, task func()) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.closed {
		return
	}

	queue, running := d.queues[key]
	if !running {
		queue = make(chan func(), queueDepth)
		d.queues[key] = queue
		d.wg.Add(1)
		go d.serve(queue)
	}

	// The send happens under the lock so it cannot race Shutdown closing the same channel.
	select {
	case queue <- task:
	default:
		log.Printf("conversation %s already has %d updates waiting; dropping this one", key, queueDepth)
	}
}

// Shutdown stops accepting updates and waits for the ones already queued, so a restart does not cut
// an approval in half. It gives up after timeout rather than hanging a shutdown forever.
func (d *Dispatcher) Shutdown(timeout time.Duration) {
	d.mu.Lock()
	if d.closed {
		d.mu.Unlock()
		return
	}
	d.closed = true
	for _, queue := range d.queues {
		close(queue)
	}
	d.mu.Unlock()

	drained := make(chan struct{})
	go func() {
		d.wg.Wait()
		close(drained)
	}()

	select {
	case <-drained:
	case <-time.After(timeout):
		log.Printf("shutdown gave up after %s with updates still running", timeout)
	}
}

func (d *Dispatcher) serve(queue chan func()) {
	defer d.wg.Done()
	for task := range queue {
		runGuarded("update", task)
	}
}

// Supervise runs fn in the current goroutine and restarts it after a panic. Nothing here recovered
// before: a bad type assertion in the notifier took the whole process down, and with it the command
// path that carries the emergency stop, without a word on the way out. It is for the loops that
// must come back — a notifier that dies silently is the failure this is here to prevent.
func Supervise(name string, fn func()) {
	for !runGuarded(name, fn) {
		time.Sleep(restartDelay)
	}
}

// GoGuarded runs fn once, in its own goroutine, and swallows a panic instead of taking the process
// down. Used where a restart would repeat a side effect — re-polling a finished turn would send its
// reply to the chat a second time.
func GoGuarded(name string, fn func()) {
	go func() {
		runGuarded(name, fn)
	}()
}

// runGuarded reports whether fn finished on its own feet. A panic is logged with its stack and
// swallowed: losing one update is survivable, losing the process is not.
func runGuarded(name string, fn func()) (finished bool) {
	defer func() {
		if recovered := recover(); recovered != nil {
			log.Printf("%s panicked: %v\n%s", name, recovered, debug.Stack())
		}
	}()

	fn()
	return true
}

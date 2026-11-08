// §spec pilar-de-browser

package launch

import (
	"errors"
	"fmt"
	"os/exec"
	"sync"
	"syscall"
	"unsafe"
)

// The Windows half of "mata-se a árvore" (spec §9.3) for the case Stop never gets to run.
//
// Stop's taskkill /T only helps when this sidecar is alive to call it. When the sidecar panics, is
// hard-killed by the daemon or loses its lifeline, chrome.exe and every helper it spawned survive,
// keep the profile locked, and the next launch over that profile hits ErrInheritedInstance. A job
// object with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE makes the kernel keep that promise instead of our
// code: every Chromium this sidecar starts is put in one job whose only handle lives in this
// process, so when this process ends, however it ends, the kernel closes the handle and terminates
// every process in the job — the whole tree, helpers included, since a child inherits its parent's
// job.
//
// The handle is created non-inheritable (nil security attributes) and is never closed: its lifetime
// IS the sidecar's lifetime, and closing it early would kill every browser this sidecar holds. Go's
// os/exec also passes children an explicit handle list, so nothing reaches Chromium either way.
//
// There is no race between the launch and the assignment. Chromium is created suspended
// (CREATE_SUSPENDED), so its main thread has not run a single instruction — and so has spawned no
// helper — when it is assigned to the job; only then is it resumed. Assigning after an ordinary
// start would leave a window in which the browser could create a child outside the job.
//
// This is nested inside whatever job the daemon put this sidecar in (process_tree::Litter in the
// núcleo), which Windows 8 and later allow. That outer job covers a dead daemon; this one covers a
// dead sidecar under a living daemon, which the outer job cannot, since the daemon holds its handle.

const (
	createSuspended     = 0x00000004 // CREATE_SUSPENDED
	processSetQuota     = 0x0100     // PROCESS_SET_QUOTA
	threadSuspendResume = 0x0002     // THREAD_SUSPEND_RESUME

	jobObjectExtendedLimitInfoClass = 9      // JobObjectExtendedLimitInformation
	jobObjectLimitKillOnJobClose    = 0x2000 // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
)

// kernel32 is a KnownDLL, so loading it by bare name cannot pick up a planted copy.
var (
	kernel32                    = syscall.NewLazyDLL("kernel32.dll")
	procCreateJobObjectW        = kernel32.NewProc("CreateJobObjectW")
	procSetInformationJobObject = kernel32.NewProc("SetInformationJobObject")
	procAssignProcessToJob      = kernel32.NewProc("AssignProcessToJobObject")
	procThread32First           = kernel32.NewProc("Thread32First")
	procThread32Next            = kernel32.NewProc("Thread32Next")
	procOpenThread              = kernel32.NewProc("OpenThread")
	procResumeThread            = kernel32.NewProc("ResumeThread")
)

// jobObjectBasicLimitInformation mirrors JOBOBJECT_BASIC_LIMIT_INFORMATION, declared the way
// x/sys/windows declares it. Go's natural alignment matches the C layout on the 64-bit targets the
// sidecar ships for (amd64, arm64); on 386 Go aligns int64 to 4 bytes and the layouts would differ.
type jobObjectBasicLimitInformation struct {
	PerProcessUserTimeLimit int64
	PerJobUserTimeLimit     int64
	LimitFlags              uint32
	MinimumWorkingSetSize   uintptr
	MaximumWorkingSetSize   uintptr
	ActiveProcessLimit      uint32
	Affinity                uintptr
	PriorityClass           uint32
	SchedulingClass         uint32
}

type ioCounters struct {
	ReadOperationCount  uint64
	WriteOperationCount uint64
	OtherOperationCount uint64
	ReadTransferCount   uint64
	WriteTransferCount  uint64
	OtherTransferCount  uint64
}

// jobObjectExtendedLimitInformation mirrors JOBOBJECT_EXTENDED_LIMIT_INFORMATION, the class that
// carries KILL_ON_JOB_CLOSE (the basic class rejects that flag).
type jobObjectExtendedLimitInformation struct {
	BasicLimitInformation jobObjectBasicLimitInformation
	IoInfo                ioCounters
	ProcessMemoryLimit    uintptr
	JobMemoryLimit        uintptr
	PeakProcessMemoryUsed uintptr
	PeakJobMemoryUsed     uintptr
}

// threadEntry32 mirrors THREADENTRY32.
type threadEntry32 struct {
	Size           uint32
	Usage          uint32
	ThreadID       uint32
	OwnerProcessID uint32
	BasePri        int32
	DeltaPri       int32
	Flags          uint32
}

// browserJob is the one job every Chromium of this sidecar joins. Created on the first launch and
// held, never closed, until the process exits.
var browserJob = sync.OnceValues(newKillOnCloseJob)

// newKillOnCloseJob creates an anonymous, non-inheritable job whose last handle closing terminates
// every process in it.
func newKillOnCloseJob() (syscall.Handle, error) {
	r, _, callErr := procCreateJobObjectW.Call(0, 0)
	if r == 0 {
		return 0, fmt.Errorf("launch: creating the browser job object: %w", callErr)
	}
	job := syscall.Handle(r)
	var limits jobObjectExtendedLimitInformation
	limits.BasicLimitInformation.LimitFlags = jobObjectLimitKillOnJobClose
	ok, _, callErr := procSetInformationJobObject.Call(
		uintptr(job),
		jobObjectExtendedLimitInfoClass,
		uintptr(unsafe.Pointer(&limits)),
		unsafe.Sizeof(limits),
	)
	if ok == 0 {
		_ = syscall.CloseHandle(job)
		return 0, fmt.Errorf("launch: setting kill-on-close on the browser job object: %w", callErr)
	}
	return job, nil
}

// startSuspended makes cmd.Start create the process with its main thread not yet running, so
// joinJob can assign it before it spawns anything.
func startSuspended(cmd *exec.Cmd) {
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	cmd.SysProcAttr.CreationFlags |= createSuspended
}

// joinJob puts the process startSuspended created into this sidecar's job and then lets it run.
//
// An error means the browser is still suspended and outside any job of ours; the caller kills it.
// Refusing is the same answer this package gives for a missing proxy or WebRTC preference: a browser
// that would outlive a crash of this sidecar is a browser that makes the next launch inherit it.
func joinJob(cmd *exec.Cmd) error {
	job, err := browserJob()
	if err != nil {
		return err
	}
	return assignAndResume(job, cmd)
}

// assignAndResume is joinJob with the job passed in, so a test can use a job of its own and close it.
//
// cmd must have been started by startSuspended and not yet waited: os.Process holds a handle to it,
// which is what keeps its pid from being reused between Start and the OpenProcess below.
func assignAndResume(job syscall.Handle, cmd *exec.Cmd) error {
	if cmd.Process == nil {
		return errors.New("launch: assigning a process that was never started")
	}
	pid := uint32(cmd.Process.Pid)
	// The two rights assignment needs and no more.
	process, err := syscall.OpenProcess(processSetQuota|syscall.PROCESS_TERMINATE, false, pid)
	if err != nil {
		return fmt.Errorf("launch: opening chromium (pid %d) to assign it to the job: %w", pid, err)
	}
	ok, _, callErr := procAssignProcessToJob.Call(uintptr(job), uintptr(process))
	// Membership is recorded on the job; this handle has done its work either way.
	_ = syscall.CloseHandle(process)
	if ok == 0 {
		return fmt.Errorf("launch: assigning chromium (pid %d) to the job: %w", pid, callErr)
	}
	return resumeProcess(pid)
}

// resumeProcess resumes the threads of a process created suspended.
//
// os/exec closes the main-thread handle CreateProcess returns, so the thread is found again by
// walking a thread snapshot. A suspended process has exactly one thread, the one CreateProcess made;
// every thread owned by pid is resumed anyway, so the count is not an assumption this relies on.
func resumeProcess(pid uint32) error {
	snapshot, err := syscall.CreateToolhelp32Snapshot(syscall.TH32CS_SNAPTHREAD, 0)
	if err != nil {
		return fmt.Errorf("launch: snapshotting threads to resume chromium (pid %d): %w", pid, err)
	}
	defer syscall.CloseHandle(snapshot)

	entry := threadEntry32{Size: uint32(unsafe.Sizeof(threadEntry32{}))}
	resumed := 0
	ok, _, callErr := procThread32First.Call(uintptr(snapshot), uintptr(unsafe.Pointer(&entry)))
	for ok != 0 {
		if entry.OwnerProcessID == pid {
			if err := resumeThread(entry.ThreadID); err != nil {
				return fmt.Errorf("launch: resuming chromium (pid %d): %w", pid, err)
			}
			resumed++
		}
		entry.Size = uint32(unsafe.Sizeof(entry))
		ok, _, callErr = procThread32Next.Call(uintptr(snapshot), uintptr(unsafe.Pointer(&entry)))
	}
	if errno, isErrno := callErr.(syscall.Errno); !isErrno || errno != syscall.ERROR_NO_MORE_FILES {
		return fmt.Errorf("launch: walking the thread snapshot for chromium (pid %d): %w", pid, callErr)
	}
	if resumed == 0 {
		return fmt.Errorf("launch: chromium (pid %d) has no thread to resume", pid)
	}
	return nil
}

func resumeThread(tid uint32) error {
	r, _, callErr := procOpenThread.Call(threadSuspendResume, 0, uintptr(tid))
	if r == 0 {
		return fmt.Errorf("opening thread %d: %w", tid, callErr)
	}
	thread := syscall.Handle(r)
	defer syscall.CloseHandle(thread)
	// ResumeThread returns the previous suspend count, or (DWORD)-1 on failure.
	previous, _, callErr := procResumeThread.Call(uintptr(thread))
	if uint32(previous) == 0xFFFFFFFF {
		return fmt.Errorf("resuming thread %d: %w", tid, callErr)
	}
	return nil
}

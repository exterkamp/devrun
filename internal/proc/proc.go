// Package proc runs plain processes as the "proc" kind: each in its own
// session/process group via setsid, with output going to a log file.
package proc

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/exterkamp/devrun/internal/registry"
	"github.com/exterkamp/devrun/internal/runner"
)

// Kind is the registry kind string for plain processes.
const Kind = "proc"

func init() { runner.Register(Runner{}) }

// Runner implements runner.Runner for plain processes.
type Runner struct{}

func (Runner) Kind() string { return Kind }

// Substitute replaces {port} in args with port.
func Substitute(args []string, port int) []string {
	out := make([]string, len(args))
	for i, a := range args {
		out[i] = strings.ReplaceAll(a, "{port}", strconv.Itoa(port))
	}
	return out
}

func (Runner) Start(spec runner.Spec) (runner.Handle, error) {
	e := spec.Entry
	if len(e.Cmd) == 0 {
		return runner.Handle{}, errors.New("empty command")
	}
	args := Substitute(e.Cmd, e.Ports[spec.Primary])
	path, err := exec.LookPath(args[0])
	if err != nil {
		return runner.Handle{}, err
	}
	if err := os.MkdirAll(filepath.Dir(e.Log), 0o755); err != nil {
		return runner.Handle{}, err
	}
	logf, err := os.OpenFile(e.Log, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
	if err != nil {
		return runner.Handle{}, err
	}
	defer logf.Close()

	cmd := exec.Command(path, args[1:]...)
	cmd.Dir = spec.Dir
	cmd.Stdin = nil
	cmd.Stdout, cmd.Stderr = logf, logf
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
	names := make([]string, 0, len(e.Ports))
	for n := range e.Ports {
		names = append(names, n)
	}
	sort.Strings(names)
	cmd.Env = os.Environ()
	for _, n := range names {
		cmd.Env = append(cmd.Env, fmt.Sprintf("%s=%d", n, e.Ports[n]))
	}
	if err := cmd.Start(); err != nil {
		return runner.Handle{}, err
	}
	pid := cmd.Process.Pid
	cmd.Process.Release() // we never Wait: the server outlives devrun
	start, _ := StartTime(pid)
	return runner.Handle{PID: pid, PGID: pid, PIDStart: start}, nil
}

func (Runner) Alive(e registry.Entry) bool {
	return e.PID > 0 && pidAlive(e.PID, e.PIDStart)
}

// groupGone reports whether no process remains in process group pgid.
func groupGone(pgid int) bool { return !groupAlive(pgid) }

func waitGroupGone(pgid int, d time.Duration) bool {
	deadline := time.Now().Add(d)
	for {
		if groupGone(pgid) {
			return true
		}
		if time.Now().After(deadline) {
			return false
		}
		time.Sleep(25 * time.Millisecond)
	}
}

// Stop sends SIGTERM to the process group, then SIGKILL after grace. It
// refuses to signal anything if the recorded PID now belongs to a different
// process (start time mismatch): that PID was recycled and is not ours.
func (Runner) Stop(e registry.Entry, grace time.Duration) error {
	if e.PID <= 0 {
		return nil
	}
	pgid := e.PGID
	if pgid <= 0 {
		pgid = e.PID
	}
	if _, start, ok := procStat(e.PID); ok {
		if start != e.PIDStart {
			return nil // PID reused by someone else; our process is long gone
		}
	} else if err := syscall.Kill(e.PID, 0); err != syscall.ESRCH {
		// An unreadable identity is not proof that the leader is gone. Refuse
		// to signal a group whose existing leader we cannot identify.
		return fmt.Errorf("cannot verify identity of process %d", e.PID)
	}
	// Leader missing is fine: the group may still hold stragglers. The kernel
	// won't recycle a pgid while the group has members.
	if err := syscall.Kill(-pgid, syscall.SIGTERM); err != nil {
		if err == syscall.ESRCH {
			return nil
		}
		return err
	}
	if groupGone(pgid) {
		return nil
	}
	if waitGroupGone(pgid, grace) {
		return nil
	}
	if err := syscall.Kill(-pgid, syscall.SIGKILL); err != nil && err != syscall.ESRCH {
		return err
	}
	if !waitGroupGone(pgid, 2*time.Second) {
		return fmt.Errorf("process group %d survived SIGKILL", pgid)
	}
	return nil
}

func (Runner) Logs(ctx context.Context, e registry.Entry, opts runner.LogOpts, out io.Writer) error {
	return TailFile(ctx, e.Log, opts.Lines, opts.Follow, out)
}

// Package hygiene finds things devrun should clean up: listeners nobody
// registered (orphans), ports a server bound instead of the one it was given
// (drift), and compose projects with no registry entry.
package hygiene

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// Listener is one process holding a LISTEN socket.
type Listener struct {
	Port  int
	PID   int
	PGID  int
	Start uint64 // platform process start time; guards against PID reuse
}

// Scanner reads native listener ownership. Tests can point Root at a fake
// /proc tree on either supported platform.
type Scanner struct{ Root string }

func (s Scanner) root() string {
	if s.Root == "" {
		return "/proc"
	}
	return s.Root
}

// listenInodes maps socket inode to port for every LISTEN socket.
func (s Scanner) listenInodes() (map[uint64]int, error) {
	out := map[uint64]int{}
	found := false
	for _, n := range []string{"net/tcp", "net/tcp6"} {
		f, err := os.Open(filepath.Join(s.root(), n))
		if err != nil {
			continue
		}
		found = true
		sc := bufio.NewScanner(f)
		sc.Scan() // header
		for sc.Scan() {
			fl := strings.Fields(sc.Text())
			if len(fl) < 10 || fl[3] != "0A" {
				continue
			}
			i := strings.LastIndexByte(fl[1], ':')
			if i < 0 {
				continue
			}
			port, err := strconv.ParseUint(fl[1][i+1:], 16, 16)
			if err != nil {
				continue
			}
			ino, err := strconv.ParseUint(fl[9], 10, 64)
			if err != nil || ino == 0 {
				continue
			}
			out[ino] = int(port)
		}
		f.Close()
	}
	if !found {
		return nil, fmt.Errorf("cannot read %s/net/tcp", s.root())
	}
	return out, nil
}

// parseStat returns state, pgrp and start time from /proc/<pid>/stat. The comm
// field may contain spaces and parens, so split after the last ')'.
func parseStat(s string) (state byte, pgrp int, start uint64, ok bool) {
	i := strings.LastIndexByte(s, ')')
	if i < 0 {
		return
	}
	f := strings.Fields(s[i+1:])
	if len(f) < 20 {
		return
	}
	g, err1 := strconv.Atoi(f[2])
	st, err2 := strconv.ParseUint(f[19], 10, 64)
	if err1 != nil || err2 != nil {
		return
	}
	return f[0][0], g, st, true
}

func (s Scanner) procStat(pid int) (state byte, pgrp int, start uint64, ok bool) {
	data, err := os.ReadFile(filepath.Join(s.root(), strconv.Itoa(pid), "stat"))
	if err != nil {
		return
	}
	return parseStat(string(data))
}

// Listeners returns every (port, process) pair with a LISTEN socket that this
// user can see. Zombies are skipped. Sorted by port, then PID.
func (s Scanner) procListeners() ([]Listener, error) {
	inodes, err := s.listenInodes()
	if err != nil {
		return nil, err
	}
	ents, err := os.ReadDir(s.root())
	if err != nil {
		return nil, err
	}
	var out []Listener
	for _, de := range ents {
		pid, err := strconv.Atoi(de.Name())
		if err != nil {
			continue
		}
		state, pgrp, start, ok := s.stat(pid)
		if !ok || state == 'Z' || state == 'X' {
			continue
		}
		fdDir := filepath.Join(s.root(), de.Name(), "fd")
		fds, err := os.ReadDir(fdDir)
		if err != nil {
			continue // not ours to see, or exited
		}
		seen := map[int]bool{}
		for _, fd := range fds {
			link, err := os.Readlink(filepath.Join(fdDir, fd.Name()))
			if err != nil || !strings.HasPrefix(link, "socket:[") {
				continue
			}
			ino, err := strconv.ParseUint(strings.TrimSuffix(strings.TrimPrefix(link, "socket:["), "]"), 10, 64)
			if err != nil {
				continue
			}
			if port, ok := inodes[ino]; ok && !seen[port] {
				seen[port] = true
				out = append(out, Listener{Port: port, PID: pid, PGID: pgrp, Start: start})
			}
		}
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Port != out[j].Port {
			return out[i].Port < out[j].Port
		}
		return out[i].PID < out[j].PID
	})
	return out, nil
}

// GroupPorts returns the sorted ports listened on by any process in pgid.
func (s Scanner) GroupPorts(pgid int) ([]int, error) {
	ls, err := s.Listeners()
	if err != nil {
		return nil, err
	}
	set := map[int]bool{}
	for _, l := range ls {
		if l.PGID == pgid {
			set[l.Port] = true
		}
	}
	ports := make([]int, 0, len(set))
	for p := range set {
		ports = append(ports, p)
	}
	sort.Ints(ports)
	return ports, nil
}

// Kill terminates the process behind l with SIGTERM, then SIGKILL after grace.
// It re-reads the process's start time first and refuses (returning false, nil)
// if the PID now belongs to a different process, and it never signals devrun
// itself. Only the one PID is signalled, not its group.
func (s Scanner) Kill(l Listener, grace time.Duration) (killed bool, err error) {
	if l.PID <= 1 || l.PID == os.Getpid() {
		return false, nil
	}
	same := func() bool {
		state, _, start, ok := s.stat(l.PID)
		return ok && state != 'Z' && state != 'X' && start == l.Start
	}
	if !same() {
		return false, nil
	}
	if err := syscall.Kill(l.PID, syscall.SIGTERM); err != nil {
		if err == syscall.ESRCH {
			return true, nil
		}
		return false, err
	}
	deadline := time.Now().Add(grace)
	for time.Now().Before(deadline) {
		if !same() {
			return true, nil
		}
		time.Sleep(25 * time.Millisecond)
	}
	if same() {
		if err := syscall.Kill(l.PID, syscall.SIGKILL); err != nil && err != syscall.ESRCH {
			return false, err
		}
	}
	return true, nil
}

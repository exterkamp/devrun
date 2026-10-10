//go:build darwin

// Package macos provides native process inspection on macOS.
package macos

import "golang.org/x/sys/unix"

// Process returns state, process group and a microsecond-resolution start
// timestamp from the kernel. Never substitute a zero or approximate timestamp:
// callers use this identity to guard against signaling recycled PIDs.
func Process(pid int) (state byte, pgid int, start uint64, ok bool) {
	if pid <= 0 {
		return
	}
	p, err := unix.SysctlKinfoProc("kern.proc.pid", pid)
	if err != nil || int(p.Proc.P_pid) != pid || p.Proc.P_stat == 0 {
		return
	}
	state = 'S'
	if p.Proc.P_stat == 5 { // SZOMB in sys/proc.h
		state = 'Z'
	}
	tv := p.Proc.P_starttime
	if tv.Sec <= 0 {
		return 0, 0, 0, false
	}
	return state, int(p.Eproc.Pgid), uint64(tv.Sec)*1_000_000 + uint64(tv.Usec), true
}

// GroupAlive excludes unreaped zombies, which kill(-pgid, 0) would include.
func GroupAlive(pgid int) bool {
	ps, err := unix.SysctlKinfoProcSlice("kern.proc.pgrp", pgid)
	if err != nil {
		return unix.Kill(-pgid, 0) != unix.ESRCH
	}
	for _, p := range ps {
		if int(p.Eproc.Pgid) == pgid && p.Proc.P_stat != 5 && p.Proc.P_stat != 0 {
			return true
		}
	}
	return false
}

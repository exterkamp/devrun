//go:build linux

package proc

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"syscall"
)

// procStat reads /proc/<pid>/stat and returns the state letter and start time
// (clock ticks since boot). ok is false if the process does not exist.
func procStat(pid int) (state byte, start uint64, ok bool) {
	data, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", pid))
	if err != nil {
		return 0, 0, false
	}
	state, start, err = parseStat(string(data))
	return state, start, err == nil
}

// groupAlive reports whether any non-zombie process is in process group pgid.
// (kill(-pgid, 0) would also count unreaped zombies, which can linger when the
// parent that should reap them is gone.)
func groupAlive(pgid int) bool {
	ents, err := os.ReadDir("/proc")
	if err != nil {
		return syscall.Kill(-pgid, 0) != syscall.ESRCH
	}
	for _, de := range ents {
		pid, err := strconv.Atoi(de.Name())
		if err != nil {
			continue
		}
		data, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", pid))
		if err != nil {
			continue
		}
		s := string(data)
		i := strings.LastIndexByte(s, ')')
		if i < 0 {
			continue
		}
		f := strings.Fields(s[i+1:])
		if len(f) < 3 || f[0] == "Z" || f[0] == "X" {
			continue
		}
		if g, err := strconv.Atoi(f[2]); err == nil && g == pgid {
			return true
		}
	}
	return false
}

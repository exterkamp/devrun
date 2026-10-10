package proc

import (
	"fmt"
	"strconv"
	"strings"
)

// parseStat extracts state (field 3) and starttime (field 22). The comm field
// may contain spaces and parens, so split after the last ')'.
func parseStat(s string) (byte, uint64, error) {
	i := strings.LastIndexByte(s, ')')
	if i < 0 {
		return 0, 0, fmt.Errorf("malformed stat")
	}
	f := strings.Fields(s[i+1:])
	if len(f) < 20 {
		return 0, 0, fmt.Errorf("short stat")
	}
	start, err := strconv.ParseUint(f[19], 10, 64)
	if err != nil {
		return 0, 0, err
	}
	return f[0][0], start, nil
}

// StartTime returns the start time of pid, or ok=false if it is not running.
func StartTime(pid int) (uint64, bool) {
	_, start, ok := procStat(pid)
	return start, ok
}

// pidAlive: the process exists, is not a zombie, and is the one we started.
func pidAlive(pid int, start uint64) bool {
	state, s, ok := procStat(pid)
	return ok && state != 'Z' && state != 'X' && s == start
}

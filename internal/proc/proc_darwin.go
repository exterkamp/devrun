//go:build darwin

package proc

import "github.com/exterkamp/devrun/internal/macos"

func procStat(pid int) (state byte, start uint64, ok bool) {
	state, _, start, ok = macos.Process(pid)
	return
}

func groupAlive(pgid int) bool { return macos.GroupAlive(pgid) }

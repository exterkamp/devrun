//go:build darwin

package hygiene

import (
	"sort"

	"github.com/exterkamp/devrun/internal/macos"
)

func (s Scanner) stat(pid int) (byte, int, uint64, bool) {
	if s.Root != "" {
		return s.procStat(pid)
	}
	return macos.Process(pid)
}

func (s Scanner) Listeners() ([]Listener, error) {
	if s.Root != "" {
		return s.procListeners()
	}
	sockets, err := macos.Listeners()
	if err != nil {
		return nil, err
	}
	var out []Listener
	for _, l := range sockets {
		state, pgid, start, ok := macos.Process(l.PID)
		if ok && state != 'Z' {
			out = append(out, Listener{Port: l.Port, PID: l.PID, PGID: pgid, Start: start})
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

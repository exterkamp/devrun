//go:build darwin

package macos

import (
	"errors"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
)

type Listener struct{ PID, Port int }

// Listeners reads machine-readable lsof fields; without sudo, only visible
// processes are reported. That is sufficient for drift and same-user cleanup.
func Listeners() ([]Listener, error) {
	out, err := exec.Command("/usr/sbin/lsof", "-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn").Output()
	if err != nil {
		var exit *exec.ExitError
		// lsof exits 1 (with no output) when no matching sockets exist.
		if !errors.As(err, &exit) || exit.ExitCode() != 1 || len(out) != 0 || len(exit.Stderr) != 0 {
			return nil, fmt.Errorf("cannot read listener ownership (lsof failed): %w", err)
		}
	}
	return parseLsof(string(out)), nil
}

func parseLsof(out string) []Listener {
	var listeners []Listener
	pid := 0
	seen := map[Listener]bool{}
	for _, line := range strings.Split(out, "\n") {
		if len(line) < 2 {
			continue
		}
		switch line[0] {
		case 'p':
			pid, _ = strconv.Atoi(line[1:])
		case 'n':
			i := strings.LastIndexByte(line, ':')
			if pid <= 0 || i < 0 {
				continue
			}
			port, err := strconv.Atoi(line[i+1:])
			if err != nil || port <= 0 || port > 65535 {
				continue
			}
			l := Listener{PID: pid, Port: port}
			if !seen[l] {
				seen[l] = true
				listeners = append(listeners, l)
			}
		}
	}
	return listeners
}

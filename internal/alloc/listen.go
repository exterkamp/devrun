package alloc

import (
	"bufio"
	"io"
	"strconv"
	"strings"
)

// ParseProcNetTCP adds the local port of every LISTEN (state 0A) socket in a
// /proc/net/tcp-formatted stream to ports.
func ParseProcNetTCP(r io.Reader, ports map[int]bool) error {
	sc := bufio.NewScanner(r)
	sc.Scan() // header
	for sc.Scan() {
		fields := strings.Fields(sc.Text())
		if len(fields) < 4 || fields[3] != "0A" {
			continue
		}
		i := strings.LastIndexByte(fields[1], ':')
		if i < 0 {
			continue
		}
		p, err := strconv.ParseUint(fields[1][i+1:], 16, 16)
		if err != nil {
			continue
		}
		ports[int(p)] = true
	}
	return sc.Err()
}

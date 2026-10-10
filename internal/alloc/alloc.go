// Package alloc hands out stable ports from the devrun range.
//
// A port is derived from a hash of "<key>#<portname>" and probed forward past
// anything already taken. "Taken" is the union of: ports in the registry, ports
// reserved for other keys, and any extra Source (host listeners by default;
// later milestones add published container ports).
package alloc

import (
	"fmt"
	"hash/fnv"
	"sync"

	"github.com/exterkamp/devrun/internal/registry"
)

const (
	MinPort = 41000
	MaxPort = 41999
)

// Source reports ports that are busy for reasons outside the registry.
type Source func() (map[int]bool, error)

var (
	srcMu         sync.Mutex
	globalSources = []Source{Listening}
)

// RegisterSource adds a Source used by every Allocator built with New. Call it
// from an init function; this is how other milestones plug in (e.g. docker).
func RegisterSource(s Source) {
	srcMu.Lock()
	defer srcMu.Unlock()
	globalSources = append(globalSources, s)
}

// Allocator picks ports. The zero Min/Max mean the default range.
type Allocator struct {
	Min, Max int
	Sources  []Source
	// Available is an optional final OS bind check for a candidate port.
	Available func(int) bool
}

// New returns an Allocator over the default range and all registered sources.
func New() *Allocator {
	srcMu.Lock()
	defer srcMu.Unlock()
	return &Allocator{Min: MinPort, Max: MaxPort, Sources: append([]Source(nil), globalSources...), Available: portAvailable}
}

func (a *Allocator) bounds() (int, int) {
	lo, hi := a.Min, a.Max
	if lo == 0 && hi == 0 {
		return MinPort, MaxPort
	}
	return lo, hi
}

// Hash is the probe start offset for a name.
func Hash(s string) uint32 {
	h := fnv.New32a()
	h.Write([]byte(s))
	return h.Sum32()
}

// Pick returns the first port at or after the hash of name (wrapping inside
// [lo,hi]) for which taken returns false.
func Pick(name string, lo, hi int, taken func(int) bool) (int, bool) {
	n := hi - lo + 1
	start := int(Hash(name) % uint32(n))
	for i := 0; i < n; i++ {
		p := lo + (start+i)%n
		if !taken(p) {
			return p, true
		}
	}
	return 0, false
}

// ReservationKey is the Reserved-map key for a port name of a registry key.
func ReservationKey(key, portName string) string { return key + "#" + portName }

// Allocate returns one port per name for key. It must be called inside
// registry.Store.Update so the lock covers allocation and the caller's write of
// the resulting entry; it records reservations on f so the same key gets the
// same port next time. Any entry already under key is ignored (it is being
// replaced).
func (a *Allocator) Allocate(f *registry.File, key string, names []string) (map[string]int, error) {
	lo, hi := a.bounds()
	busy := map[int]bool{}
	for _, src := range a.Sources {
		m, err := src()
		if err != nil {
			return nil, err
		}
		for p := range m {
			busy[p] = true
		}
	}
	for p := range f.PortsInUse(key) {
		busy[p] = true
	}
	// Ports promised to other keys: avoided while the range has room, so a
	// stopped server's port isn't stolen and stability holds.
	reservedByOthers := map[int]bool{}
	for rk, p := range f.Reserved {
		own := false
		for _, n := range names {
			if rk == ReservationKey(key, n) {
				own = true
			}
		}
		if !own {
			reservedByOthers[p] = true
		}
	}

	out := map[string]int{}
	chosen := map[int]bool{}
	for _, n := range names {
		rk := ReservationKey(key, n)
		free := func(p int) bool { return !busy[p] && !chosen[p] && (a.Available == nil || a.Available(p)) }
		port := 0
		if p, ok := f.Reserved[rk]; ok && p >= lo && p <= hi && free(p) {
			port = p
		}
		if port == 0 {
			if p, ok := Pick(rk, lo, hi, func(p int) bool { return !free(p) || reservedByOthers[p] }); ok {
				port = p
			} else if p, ok := Pick(rk, lo, hi, func(p int) bool { return !free(p) }); ok {
				port = p
			}
		}
		if port == 0 {
			return nil, fmt.Errorf("no free port in %d-%d", lo, hi)
		}
		chosen[port] = true
		out[n] = port
		if f.Reserved == nil {
			f.Reserved = map[string]int{}
		}
		f.Reserved[rk] = port
	}
	return out, nil
}

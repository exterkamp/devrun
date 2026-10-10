//go:build linux

package hygiene

func (s Scanner) stat(pid int) (byte, int, uint64, bool) { return s.procStat(pid) }
func (s Scanner) Listeners() ([]Listener, error)         { return s.procListeners() }

//go:build darwin

package alloc

import (
	"fmt"
	"net"

	"github.com/exterkamp/devrun/internal/macos"
)

// Listening reports the TCP listeners visible to the invoking user.
func Listening() (map[int]bool, error) {
	listeners, err := macos.Listeners()
	if err != nil {
		return nil, err
	}
	ports := map[int]bool{}
	for _, l := range listeners {
		ports[l.Port] = true
	}
	return ports, nil
}

// portAvailable also catches sockets belonging to users lsof cannot inspect.
// Probe wildcard and each local address: BSD allows a SO_REUSEADDR wildcard
// bind beside an address-specific listener, so the exact addresses matter.
// Keep SO_REUSEADDR enabled to permit restarting a server with TIME_WAIT sockets.
func portAvailable(port int) bool {
	hosts := map[string]string{"0.0.0.0": "tcp4", "127.0.0.1": "tcp4", "::": "tcp6", "::1": "tcp6"}
	ifaces, err := net.Interfaces()
	if err != nil {
		return false
	}
	for _, iface := range ifaces {
		if iface.Flags&net.FlagUp == 0 {
			continue
		}
		addrs, err := iface.Addrs()
		if err != nil {
			return false
		}
		for _, addr := range addrs {
			ip, _, err := net.ParseCIDR(addr.String())
			if err != nil {
				continue
			}
			host, network := ip.String(), "tcp6"
			if ip.To4() != nil {
				network = "tcp4"
			} else if ip.IsLinkLocalUnicast() {
				host += "%" + iface.Name
			}
			hosts[host] = network
		}
	}
	for host, network := range hosts {
		ln, err := net.Listen(network, net.JoinHostPort(host, fmt.Sprint(port)))
		if err != nil {
			return false
		}
		ln.Close()
	}
	return true
}

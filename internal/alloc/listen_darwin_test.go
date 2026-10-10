//go:build darwin

package alloc

import (
	"fmt"
	"net"
	"testing"

	"github.com/exterkamp/devrun/internal/registry"
)

func TestAllocateProbesPortsMissingFromListenerSnapshot(t *testing.T) {
	for _, network := range []string{"tcp4", "tcp6"} {
		t.Run(network, func(t *testing.T) {
			host := "127.0.0.1"
			if network == "tcp6" {
				host = "::1"
			}
			var ln net.Listener
			var port int
			for p := MinPort; p < MaxPort; p++ {
				l, err := net.Listen(network, net.JoinHostPort(host, fmt.Sprint(p)))
				if err == nil {
					ln, port = l, p
					break
				}
			}
			if ln == nil {
				t.Fatal("no free test port")
			}
			defer ln.Close()
			a := New()
			a.Sources = []Source{none} // listener ownership snapshot missed this socket
			a.Min, a.Max = port, port+1
			f := &registry.File{Reserved: map[string]int{ReservationKey("test", "PORT"): port}}
			ports, err := a.Allocate(f, "test", []string{"PORT"})
			if err != nil || ports["PORT"] != port+1 {
				t.Fatalf("allocated %v: %v", ports, err)
			}
		})
	}
}

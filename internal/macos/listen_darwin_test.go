//go:build darwin

package macos

import (
	"os"
	"syscall"
	"testing"
)

func TestParseLsof(t *testing.T) {
	got := parseLsof("p123\nf4\nn127.0.0.1:41237\nn[::1]:41237\nn*:41555\np456\nn[::]:41999\npbad\nn*:41000\np789\nn*:bad\nn*:70000\n")
	want := []Listener{{123, 41237}, {123, 41555}, {456, 41999}}
	if len(got) != len(want) {
		t.Fatalf("listeners: %v", got)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("listeners: %v", got)
		}
	}
}

func TestProcessIdentity(t *testing.T) {
	state, group, start, ok := Process(os.Getpid())
	if !ok || state == 'Z' || group != syscall.Getpgrp() || start == 0 {
		t.Fatalf("identity: %c %d %d %v", state, group, start, ok)
	}
	_, _, again, ok := Process(os.Getpid())
	if !ok || start != again {
		t.Fatal("process identity changed")
	}
	if !GroupAlive(group) {
		t.Fatal("own group not alive")
	}
	for _, pid := range []int{-1, 0, 1 << 30} {
		if _, _, _, ok := Process(pid); ok {
			t.Fatalf("invalid pid %d exists", pid)
		}
	}
}

package main

import (
	"fmt"
	"os"
	"golang.org/x/term"
)

func main() {
	fmt.Println("=== Terminal Test ===")

	// Check isatty for stdin, stdout, stderr
	fmt.Println("\n=== isatty() checks ===")
	fds := []struct {
		name string
		fd   uintptr
	}{
		{"stdin", os.Stdin.Fd()},
		{"stdout", os.Stdout.Fd()},
		{"stderr", os.Stderr.Fd()},
	}

	for _, f := range fds {
		// Use term.IsTerminal which calls isatty internally
		if term.IsTerminal(int(f.fd)) {
			fmt.Printf("✓ %s is a terminal (fd=%d)\n", f.name, f.fd)
		} else {
			fmt.Printf("✗ %s is NOT a terminal (fd=%d)\n", f.name, f.fd)
		}
	}

	// Get terminal attributes
	fmt.Println("\n=== Terminal Attributes ===")
	fd := int(os.Stdin.Fd())
	if term.IsTerminal(fd) {
		// Try to get state
		state, err := term.GetState(fd)
		if err != nil {
			fmt.Printf("Failed to get terminal state: %v\n", err)
		} else {
			fmt.Printf("Got terminal state: %+v\n", state)
		}

		// Check if we can make raw
		rawState, err := term.MakeRaw(fd)
		if err != nil {
			fmt.Printf("Failed to make raw: %v\n", err)
		} else {
			fmt.Println("Can set raw mode")
			term.Restore(fd, rawState)
		}
	}

	// Check TERM environment variable
	fmt.Println("\n=== Environment Variables ===")
	termEnv := os.Getenv("TERM")
	if termEnv != "" {
		fmt.Printf("TERM=%s\n", termEnv)
	} else {
		fmt.Println("TERM is not set")
	}

	// Try to get window size
	fmt.Println("\n=== Window Size ===")
	if width, height, err := term.GetSize(fd); err == nil {
		fmt.Printf("Terminal size: %dx%d\n", width, height)
	} else {
		fmt.Printf("Failed to get terminal size: %v\n", err)
	}

	// Check if we're in a container or special environment
	fmt.Println("\n=== Additional Checks ===")

	// Check for common container indicators
	for _, env := range []string{
		"CONTAINER",
		"DOCKER",
		"KUBERNETES_SERVICE_HOST",
		"TMUX",
		"SCREEN",
	} {
		if val := os.Getenv(env); val != "" {
			fmt.Printf("%s=%s\n", env, val)
		}
	}

	// Try to read some input to see if it's buffered
	fmt.Println("\n=== Input Test ===")
	fmt.Println("Type a few characters then Enter...")

	var buf [256]byte
	n, err := os.Stdin.Read(buf[:])
	if err != nil {
		fmt.Printf("Read error: %v\n", err)
	} else {
		fmt.Printf("Read %d bytes: %q\n", n, buf[:n])
		for i := 0; i < n; i++ {
			fmt.Printf("  [%d] = 0x%02x\n", i, buf[i])
		}
	}

	fmt.Println("\n=== Test Complete ===")
}
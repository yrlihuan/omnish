package main

import (
	"fmt"
	"os"
	"sort"
)

func main() {
	fmt.Println("=== Environment Variables ===")

	// Get all environment variables
	env := os.Environ()
	sort.Strings(env)

	// Print important ones first
	important := []string{
		"TERM", "PATH", "HOME", "USER", "SHELL",
		"TMUX", "SSH_TTY", "COLORTERM",
		"OMNISH_SESSION_ID", "OMNISH_SOCKET",
	}

	fmt.Println("\n=== Important Variables ===")
	for _, key := range important {
		if val := os.Getenv(key); val != "" {
			fmt.Printf("%s=%s\n", key, val)
		}
	}

	fmt.Println("\n=== All Variables (first 20) ===")
	count := 0
	for _, e := range env {
		if count >= 20 {
			break
		}
		// Skip long values
		if len(e) > 100 {
			e = e[:100] + "..."
		}
		fmt.Println(e)
		count++
	}

	// Check if TERM is set
	if term := os.Getenv("TERM"); term == "" {
		fmt.Println("\n⚠️  WARNING: TERM environment variable is not set!")
		fmt.Println("This can cause terminal detection problems.")
	} else {
		fmt.Printf("\n✓ TERM is set to: %s\n", term)
	}

	// Check if we're in omnish
	if os.Getenv("OMNISH_SESSION_ID") != "" {
		fmt.Println("\n✓ Running inside omnish")
	}
}
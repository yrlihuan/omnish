package main

import (
	"fmt"
	"os"
	"golang.org/x/term"
)

func main() {
	fmt.Println("=== Terminal Diagnostics ===")

	// Check if stdin is a terminal
	fd := int(os.Stdin.Fd())
	if !term.IsTerminal(fd) {
		fmt.Println("ERROR: stdin is not a terminal")
		os.Exit(1)
	}

	fmt.Println("✓ stdin is a terminal")

	// Get current terminal state
	oldState, err := term.GetState(fd)
	if err != nil {
		fmt.Printf("ERROR: Failed to get terminal state: %v\n", err)
		os.Exit(1)
	}

	// Check various terminal attributes
	fmt.Println("\n=== Terminal Attributes ===")

	// Try to get raw mode state
	// Note: term package doesn't expose raw mode flag directly
	// We'll check by trying to make raw and restore
	fmt.Println("Checking raw mode detection...")

	// Make raw temporarily to test
	rawState, err := term.MakeRaw(fd)
	if err != nil {
		fmt.Printf("ERROR: Failed to set raw mode: %v\n", err)
	} else {
		fmt.Println("✓ Can set raw mode")
		// Restore immediately
		term.Restore(fd, rawState)
	}

	// Restore original state
	term.Restore(fd, oldState)

	fmt.Println("\n=== Input Test ===")
	fmt.Println("Press an arrow key (↑↓←→) or ESC to test, then Enter...")

	// Read single byte to check input handling
	var buf [1]byte
	n, err := os.Stdin.Read(buf[:])
	if err != nil {
		fmt.Printf("ERROR: Failed to read input: %v\n", err)
		os.Exit(1)
	}

	fmt.Printf("Read %d byte(s): %q (hex: %02x)\n", n, buf[:n], buf[:n])
	if n > 0 && buf[0] == 0x1b {
		fmt.Println("Detected ESC character (0x1b)")
		fmt.Println("Note: Arrow keys typically send ESC [A, ESC [B, etc.")

		// Try to read more bytes for escape sequence
		var seq [10]byte
		seq[0] = buf[0]
		m, _ := os.Stdin.Read(seq[1:])
		fmt.Printf("Escape sequence: %q (hex:", seq[:1+m])
		for i := 0; i < 1+m; i++ {
			fmt.Printf(" %02x", seq[i])
		}
		fmt.Println(")")
	}

	fmt.Println("\n=== Recommendations ===")
	fmt.Println("1. If you see [A, [B, etc. displayed, terminal may not be processing escape sequences")
	fmt.Println("2. Check if terminal is in raw mode or has special settings")
	fmt.Println("3. Try running from a regular shell outside of omnish")
}
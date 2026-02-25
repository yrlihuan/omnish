package utils_test

import (
	"testing"

	"github.com/yrlihuan/omnish/tools/go_survey_tool/pkg/utils"
)

func TestIsEmpty(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		expected bool
	}{
		{"Empty string", "", true},
		{"Only spaces", "   ", true},
		{"Only tabs", "\t\t\t", true},
		{"Mixed whitespace", " \t \n ", true},
		{"Non-empty", "hello", false},
		{"Non-empty with spaces", " hello world ", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := utils.IsEmpty(tt.input)
			if result != tt.expected {
				t.Errorf("IsEmpty(%q) = %v, want %v", tt.input, result, tt.expected)
			}
		})
	}
}

func TestFormatOptions(t *testing.T) {
	options := []string{"Option A", "Option B", "Option C"}
	expected := "1. Option A\n2. Option B\n3. Option C\n"

	result := utils.FormatOptions(options)
	if result != expected {
		t.Errorf("FormatOptions() = %q, want %q", result, expected)
	}
}

func TestValidateNotEmpty(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		hasError bool
	}{
		{"Empty string", "", true},
		{"Only spaces", "   ", true},
		{"Valid input", "test", false},
		{"Valid with spaces", " test ", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := utils.ValidateNotEmpty(tt.input)
			hasError := err != nil
			if hasError != tt.hasError {
				t.Errorf("ValidateNotEmpty(%q) error = %v, want error = %v", tt.input, hasError, tt.hasError)
			}
		})
	}
}
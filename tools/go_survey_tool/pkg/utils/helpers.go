package utils

import (
	"fmt"
	"os"
	"strings"
)

// PrintError 打印错误信息
func PrintError(err error) {
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
	}
}

// IsEmpty 检查字符串是否为空或仅包含空格
func IsEmpty(s string) bool {
	return strings.TrimSpace(s) == ""
}

// ValidateNotEmpty 验证输入不为空
func ValidateNotEmpty(input string) error {
	if IsEmpty(input) {
		return fmt.Errorf("value cannot be empty")
	}
	return nil
}

// FormatOptions 格式化选项列表用于显示
func FormatOptions(options []string) string {
	var builder strings.Builder
	for i, option := range options {
		builder.WriteString(fmt.Sprintf("%d. %s\n", i+1, option))
	}
	return builder.String()
}
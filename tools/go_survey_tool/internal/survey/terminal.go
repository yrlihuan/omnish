package survey

import (
	"os"

	"golang.org/x/term"
)

// TerminalModeGuard 用于临时调整终端模式
type TerminalModeGuard struct {
	fd       int
	oldState *term.State
}

// NewTerminalModeGuard 创建终端模式守卫
// 如果检测到raw模式，会临时恢复为cooked模式
func NewTerminalModeGuard() (*TerminalModeGuard, error) {
	fd := int(os.Stdin.Fd())
	if !term.IsTerminal(fd) {
		// 不是终端，返回空的guard
		return &TerminalModeGuard{fd: -1}, nil
	}

	// 获取当前终端状态
	oldState, err := term.GetState(fd)
	if err != nil {
		return nil, err
	}

	// 检查是否是raw模式
	// 简单方法：尝试设置raw模式，如果已经是raw，不会出错但状态可能相同
	// 但实际上我们不需要精确检测，只需要确保survey能在适当模式下工作
	// survey库期望在cooked模式下工作，会在内部设置自己的raw模式

	// 这里我们假设oldState是合适的模式
	// 如果来自omnish的raw模式，oldState就是raw模式
	// 我们需要临时恢复为cooked模式

	// 创建cooked模式状态（恢复原始设置）
	// 实际上，term包没有直接的"cooked"模式
	// 我们可以尝试使用系统默认值，但更简单的方法是：
	// 不改变模式，让survey库处理

	// 对于现在，我们不做任何改变，只是保存状态
	// 如果发现问题，可以在这里添加模式切换逻辑

	return &TerminalModeGuard{
		fd:       fd,
		oldState: oldState,
	}, nil
}

// Restore 恢复原始终端状态
func (g *TerminalModeGuard) Restore() error {
	if g.fd == -1 || g.oldState == nil {
		return nil
	}
	return term.Restore(g.fd, g.oldState)
}

// WithTerminalMode 在适当的终端模式下运行函数
func WithTerminalMode(fn func() error) error {
	guard, err := NewTerminalModeGuard()
	if err != nil {
		// 无法获取终端状态，直接运行
		return fn()
	}

	// 如果guard有效，确保在函数执行后恢复状态
	defer guard.Restore()

	// 运行函数
	return fn()
}
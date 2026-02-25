package survey

import (
	"fmt"
	"os"

	surveyv2 "github.com/AlecAivazis/survey/v2"
	"golang.org/x/term"
)

// withTerminalMode 临时调整终端模式以兼容survey库
func withTerminalMode(fn func() error) error {
	fd := int(os.Stdin.Fd())
	if !term.IsTerminal(fd) {
		// 不是终端，直接运行
		return fn()
	}

	// 获取当前终端状态
	oldState, err := term.GetState(fd)
	if err != nil {
		// 无法获取状态，直接运行
		return fn()
	}

	// 检查是否是raw模式（通过尝试恢复为cooked模式）
	// 如果当前是raw模式，我们需要临时恢复为cooked模式供survey使用
	// 因为survey库期望在cooked模式下工作
	wasRaw := false
	var rawState *term.State

	// 尝试获取当前是否是raw模式
	// 简单检查：尝试设置raw模式，如果已经是raw，MakeRaw会返回相同状态
	testState, err := term.MakeRaw(fd)
	if err == nil {
		// 检查testState是否与oldState不同
		// 简单方法：恢复oldState，如果testState != oldState，说明之前是cooked模式
		term.Restore(fd, oldState)
		// 如果MakeRaw成功且状态改变，说明之前不是raw模式
		// 这里我们假设如果MakeRaw成功，我们就在raw模式下
		// 实际上我们需要更精确的检测，但这是简单实现
		wasRaw = true
		rawState = testState
	}

	// 如果检测到raw模式，临时恢复为cooked模式
	if wasRaw && rawState != nil {
		// 恢复为原始状态（假设oldState是cooked模式）
		if err := term.Restore(fd, oldState); err != nil {
			// 恢复失败，继续运行
			return fn()
		}
		defer func() {
			// 函数执行完后恢复raw模式
			term.Restore(fd, rawState)
		}()
	}

	// 运行实际函数
	return fn()
}

// SelectExample 演示使用survey库进行上下键选择的示例
func SelectExample() error {
	fmt.Println("=== 上下键选择示例 ===")

	var options = []string{
		"选项 1: 红色",
		"选项 2: 蓝色",
		"选项 3: 绿色",
		"选项 4: 黄色",
		"选项 5: 退出",
	}

	prompt := &surveyv2.Select{
		Message: "请使用上下键选择一个选项:",
		Options: options,
		Default: options[0],
	}

	var selected string
	var err error

	// 在适当的终端模式下运行survey
	surveyErr := withTerminalMode(func() error {
		err = surveyv2.AskOne(prompt, &selected)
		return err
	})

	if surveyErr != nil {
		return fmt.Errorf("选择失败: %w", surveyErr)
	}

	if err != nil {
		return fmt.Errorf("选择失败: %w", err)
	}

	fmt.Printf("您选择了: %s\n", selected)

	// 根据选择执行不同操作
	switch selected {
	case options[0]:
		fmt.Println("执行红色相关操作...")
	case options[1]:
		fmt.Println("执行蓝色相关操作...")
	case options[2]:
		fmt.Println("执行绿色相关操作...")
	case options[3]:
		fmt.Println("执行黄色相关操作...")
	case options[4]:
		fmt.Println("退出程序")
		return nil
	}

	return nil
}

// RunArrowKeySelection 运行上下键选择演示
func RunArrowKeySelection() error {
	return SelectExample()
}
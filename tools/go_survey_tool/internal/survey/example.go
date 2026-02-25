package survey

import (
	"fmt"

	surveyv2 "github.com/AlecAivazis/survey/v2"
)

// ExampleSurvey 展示基本的survey使用示例
func ExampleSurvey() error {
	fmt.Println("=== Survey 示例 ===")

	// 1. 文本输入
	var name string
	err := surveyv2.AskOne(&surveyv2.Input{
		Message: "What is your name?",
	}, &name)
	if err != nil {
		return fmt.Errorf("名称输入失败: %w", err)
	}

	// 2. 选择
	var color string
	colors := []string{"Red", "Blue", "Green", "Yellow"}
	err = surveyv2.AskOne(&surveyv2.Select{
		Message: "Choose a color:",
		Options: colors,
		Default: colors[1],
	}, &color)
	if err != nil {
		return fmt.Errorf("颜色选择失败: %w", err)
	}

	// 3. 确认
	var confirm bool
	err = surveyv2.AskOne(&surveyv2.Confirm{
		Message: "Do you like Go?",
		Default: true,
	}, &confirm)
	if err != nil {
		return fmt.Errorf("确认失败: %w", err)
	}

	fmt.Printf("\nHello %s! You chose %s and ", name, color)
	if confirm {
		fmt.Println("you like Go!")
	} else {
		fmt.Println("you don't like Go.")
	}

	return nil
}

// RunInteractiveSurvey 运行交互式调查
func RunInteractiveSurvey() error {
	fmt.Println("=== Interactive Survey Example ===")
	return ExampleSurvey()
}

// CreateSurveyQuestions 创建调查问题（用于测试）
func CreateSurveyQuestions() []struct {
	Name    string
	Message string
} {
	return []struct {
		Name    string
		Message string
	}{
		{"name", "What is your name?"},
		{"color", "Choose a color:"},
		{"confirm", "Do you like Go?"},
	}
}
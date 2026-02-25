package survey_test

import (
	"testing"

	"github.com/yrlihuan/omnish/tools/go_survey_tool/internal/survey"
)

func TestRunInteractiveSurvey(t *testing.T) {
	// 这是一个示例测试，实际测试需要模拟用户输入
	// 由于survey库需要交互式输入，这里只测试函数是否存在
	t.Run("FunctionExists", func(t *testing.T) {
		// 确保函数可以调用（虽然会失败因为没有终端）
		// 在实际测试中，应该使用模拟或测试模式
		t.Skip("Survey tests require interactive terminal or mocking")
	})
}

func TestCreateSurveyQuestions(t *testing.T) {
	t.Run("QuestionsCreated", func(t *testing.T) {
		questions := survey.CreateSurveyQuestions()
		if len(questions) == 0 {
			t.Error("Expected at least one survey question")
		}

		// 检查问题结构
		for i, q := range questions {
			if q.Name == "" {
				t.Errorf("Question %d missing Name field", i)
			}
			if q.Message == "" {
				t.Errorf("Question %d missing Message field", i)
			}
		}
	})
}
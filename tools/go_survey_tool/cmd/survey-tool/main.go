package main

import (
	"fmt"
	"os"

	"github.com/yrlihuan/omnish/tools/go_survey_tool/internal/survey"
	"github.com/yrlihuan/omnish/tools/go_survey_tool/pkg/utils"
)

func main() {
	fmt.Println("=== Go Survey Tool ===")

	// 检查命令行参数
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "example", "demo":
			fmt.Println("Running survey example...")
			err := survey.RunInteractiveSurvey()
			if err != nil {
				utils.PrintError(err)
				os.Exit(1)
			}
		case "arrow", "select":
			fmt.Println("Running arrow key selection example...")
			err := survey.RunArrowKeySelection()
			if err != nil {
				utils.PrintError(err)
				os.Exit(1)
			}
		case "help", "-h", "--help":
			printHelp()
		default:
			fmt.Printf("Unknown command: %s\n", os.Args[1])
			printHelp()
		}
	} else {
		// 默认运行示例
		fmt.Println("No command specified. Running example survey...")
		err := survey.RunInteractiveSurvey()
		if err != nil {
			utils.PrintError(err)
			os.Exit(1)
		}
	}

	fmt.Println("\nSurvey tool execution completed!")
}

func printHelp() {
	fmt.Print(`
Usage:
  survey-tool [command]

Commands:
  example, demo    Run interactive survey example
  arrow, select    Run arrow key selection example
  help, -h, --help Show this help message

Examples:
  survey-tool example    Run the survey example
  survey-tool arrow      Run arrow key selection example
  survey-tool            Run default example (same as 'example')
`)
}
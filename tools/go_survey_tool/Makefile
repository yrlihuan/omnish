# Go Survey Tool Makefile

.PHONY: all build test clean help

# 默认目标
all: build

# 构建项目
build:
	@echo "Building survey-tool..."
	@cd cmd/survey-tool && go build -o ../../survey-tool

# 运行测试
test:
	@echo "Running tests..."
	@go test ./pkg/utils/... ./internal/survey/...

# 清理构建文件
clean:
	@echo "Cleaning up..."
	@rm -f survey-tool
	@rm -rf dist/

# 显示帮助
help:
	@echo "Go Survey Tool Makefile"
	@echo ""
	@echo "Usage:"
	@echo "  make build    - Build the survey tool"
	@echo "  make test     - Run tests"
	@echo "  make clean    - Clean build artifacts"
	@echo "  make all      - Build the tool (default)"
	@echo "  make help     - Show this help message"

# 开发模式：构建并运行示例
dev: build
	@echo "Running survey tool..."
	@./survey-tool example
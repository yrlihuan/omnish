# Go Survey Tool

一个基于Go语言的交互式命令行调查工具，使用survey库实现丰富的终端交互体验。

## 功能特性

- 支持多种交互式组件（输入框、选择器、确认框等）
- 支持上下键导航选择
- 支持表单验证
- 可扩展的插件架构
- 易于集成的API

## 目录结构

```
go_survey_tool/
├── cmd/
│   └── survey-tool/
│       └── main.go          # 主程序入口
├── internal/
│   └── survey/
│       └── (内部实现代码)
├── pkg/
│   └── utils/
│       └── (公共工具代码)
├── test/
│   └── (测试文件)
└── README.md                # 本文档
```

## 快速开始

### 构建

```bash
cd go_survey_tool/cmd/survey-tool
go build -o survey-tool
```

### 运行

```bash
./survey-tool
```

## 依赖

- Go 1.22+
- [survey](https://github.com/AlecAivazis/survey) - 交互式终端UI库

## 开发计划

1. ✅ 基础目录结构
2. ⬜ 添加survey库依赖
3. ⬜ 实现基础交互组件
4. ⬜ 添加测试程序
5. ⬜ 集成到omnish项目

## 许可证

MIT
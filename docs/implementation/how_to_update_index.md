## 更新 index.md
更新index.md. 该文档描述了模块文档中中主要组件的功能和模块文档中该功能部分的行号范围.
对于每个组件, 应该主要介绍该组件的功能指责范围. 这部分描述应该覆盖所有子功能的要点.
不要太多冗余信息, 控制文件的大小, index.md会成为所有任务重llm都会加载的reference.
做增量更新的时候, 不用一一涵盖模块文档中每一处更新. 一些实现的细节忽略.

## 使用 split_doc_sections.sh 逐段更新

可以使用 `split_doc_sections.sh` 将模块文档按 `## ` 标题拆分为独立段落，便于逐段读取并更新 index.md：

```bash
# 列出所有段落及行号范围
bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-daemon.md list

# 按名称获取段落（子串匹配）
bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-daemon.md get "插件系统"

# 按编号获取段落（0=preamble, 1-N=各段落）
bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-daemon.md get 3
```


# 如何更新实现文档

## 检查哪些文档需要更新

运行 `check_doc_updates.sh` 脚本，它会对比每个文档的最后编辑commit和对应模块的最新commit：

```bash
bash docs/implementation/check_doc_updates.sh
```

## 更新流程

1. 对于每个需要更新的文档，先通过git获取文档上次编辑的commit，然后获取对应模块自该commit以来的提交日志
2. 有重要改动时读取改动的源码内容
3. 更新文档，先不提交
4. 将每个文档的更新做成task，使用subagent来并行完成更新
5. subagent可能因权限被拒绝而无法写入文件，此时需要根据其输出手动应用更改
6. 所有文档更新完成后，统一提交



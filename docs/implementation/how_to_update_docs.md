使用subagent skill, 将每个文档分发给子任务执行

在每个sugagent中:
对于每个文档, 先通过git获取文档上次编辑的commit, 然后获取对应模块的commit
通过git读取两个commit中间的提交日志. 有重要改动时读取改动内容
更新文档, 先不提交

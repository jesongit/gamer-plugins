# 通知助手（gamer-notify）

内置执行插件，首次接入[企微连](https://wx.posase.net/docs)。需要包含本插件 host 实现的新宿主。通道在工作台「通知助手」全局管理，与配置包无关，可添加多个实例，设置默认通道、启停、编辑、删除及测试。

任务编辑页始终提供「运行结果通知」，可分别配置成功、失败、取消、跳过的通道和文案。默认关闭；打开后默认选择成功和失败，仍须选择通道。定时运行和任务测试共用配置。插件缺失、停用或通道不可用时保留配置，跳过发送并记录警告，不改变任务结果；以后安装只影响未来运行。

文案变量：`{{task.name}}`、`{{task.id}}`、`{{device.name}}`、`{{device.id}}`、`{{result}}`、`{{time}}`、`{{elapsed}}`、`{{error}}`、`{{entrypoint}}`、`{{run.id}}`。留空使用默认摘要。变量仅替换一次，未知变量拒绝发送。任务保存于通用 `extensions["gamer-notify"]` 字段，Core 不解释策略。

自动化通过 gamer-yaml 的普通函数调用：

```yaml
run:
  - notify:
      title: 备份完成
      content: 数据库备份已完成
      channel: wechat
    as: delivery
  - log: 脚本继续执行
```

`channel` 缺省使用全局默认。返回 `{accepted,id?,status,reason}`；`accepted` 仅表示提交，实际结果查看发送记录。通知函数始终可编辑、校验和运行，gamer-notify 是 gamer-yaml 的可选依赖，发送另需 `notify.send` 权限。自动化步骤与任务结果通知独立发送。

标题、换行和正文合计上限 2048 个 UTF-8 字节。后台队列最多 128，4 个发送并发；不自动重试、不补发历史。状态包括 queued/sending/sent/partial/pending/failed/unknown/skipped。sent 仅代表企业微信接口接受，不保证手机送达。unknown、pending、partial 获得远端 ID 时可手动查询；查询失败保留原发送状态。

全局数据在 `data/extension-data/gamer-notify/`：Windows 密钥以当前账户 DPAPI 保护，Unix 使用仅属主可读文件；API 只返回 `has_key`，编辑留空保留原密钥。卸载保留配置和记录，配置包导入导出不包含通道。记录保留 30 天、最多 1000 条，界面读取最近 200 条。

公开原生动作：`channels.read/save/delete/default`、`notification.send`、`records.read/query`。通过已认证 `POST /api/extensions/gamer-notify/call` 调用，每次检查插件运行状态与权限。只有 notification.send 对 gamer-yaml 开放受控跨插件调用，不开放通道管理或任意网络请求。

独立构建：`./build.ps1`；主仓打包入口：`tools/build-plugins.ps1 -Plugin gamer-notify`。UI 联调：`node tools/build-plugin-ui.mjs gamer-notify`。

# 通知助手（gamer-notify）

内置执行插件，首次接入[企微连](https://wx.posase.net/docs)。需要包含本插件 host 实现的新宿主。通道在工作台「通知助手」全局管理，与配置包无关，可添加多个实例，设置默认通道、启停、编辑、删除及测试。

面板分为「通知通道」「手动发送」「发送记录」三个页签。通道编辑采用紧凑双列表单，正文占整行；切换页签保留未发送的草稿。

任务编辑页始终提供「运行结果通知」，可分别配置成功、失败、取消、跳过的通道和文案。默认关闭；打开后默认选择成功和失败，仍须选择通道。定时运行和任务测试共用配置。插件缺失、停用或通道不可用时保留配置，跳过发送并记录警告，不改变任务结果；以后安装只影响未来运行。

文案变量：`{{task.name}}`、`{{task.id}}`、`{{target.name}}`、`{{target.id}}`、`{{device.name}}`、`{{device.id}}`、`{{result}}`、`{{time}}`、`{{elapsed}}`、`{{error}}`、`{{entrypoint}}`、`{{run.id}}`。目标名称同时支持 Android 设备和浏览器目标；`device.name/id` 也解析为当前目标名称/ID。留空使用默认摘要，默认标题与正文带目标名称，目标已删除时回落到 ID。变量仅替换一次，未知变量拒绝发送。任务保存于通用 `extensions["gamer-notify"]` 字段，Core 不解释策略。

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

`notify` 还支持 `when: success|failure` 登记当前脚本或函数的结果通知，以及布尔 `enabled`（默认 true）。这种显式结果策略覆盖该脚本已有的全局和任务默认规则，关闭的结果也不会被默认规则补发；执行准备阶段尚未登记策略的错误仍由默认规则处理。跨脚本调用按被调用脚本的参数发送，同一次失败优先通知最具体的子作用域。Gamer 注入可信目标 ID，通知助手按 Android/浏览器配置名称补标题和正文，目标已删除时使用 ID。

结果通知未显式指定 `channel` 时，沿用此源码作用域匹配的启用脚本结果规则中的全部通道，无匹配规则才使用全局默认。源码作用域由 Gamer 从冻结的运行上下文注入；被调用脚本不继承调用方的规则，用户原有默认通道不改变。

手动脚本测试与定时任务也可按脚本订阅统一运行结果。`runs.read/save/delete` 管理全局规则（`expected_version` 乐观并发；`rule` 含 `package_id`、`entrypoint`、`runner_ids`、`channels`、`enabled`）。规则在 Core 完成活动清理并归档后发送成功、失败（包括等待超时与解释器异常）、取消或 panic 结果；私有版本使用 `gamer-yaml-version`，已发布脚本使用 `gamer-yaml`。不会向不匹配的配置包或入口发送；同一运行/通道去重，显式任务通知已覆盖的通道不重复发送。规则保存在通知插件全局数据中，YAML 不新增异常处理关键字。服务进程被强制杀死或机器掉电时不能发送即时通知。

标题、换行和正文合计上限 2048 个 UTF-8 字节。后台队列最多 128，4 个发送并发；不自动重试、不补发历史。状态包括 queued/sending/sent/partial/pending/failed/unknown/skipped。sent 仅代表企业微信接口接受，不保证手机送达。unknown、pending、partial 获得远端 ID 时可手动查询；查询失败保留原发送状态。

当前 Gamer 发送文本通知，不支持附运行截图。企微连本身支持 `news` 图文消息与 `image_url`，需要可访问的 HTTP(S) 图片链接；本地截图路径和需登录的运行证据接口不能直接作为接收端图片链接。

全局数据在 `data/extension-data/gamer-notify/`：Windows 密钥以当前账户 DPAPI 保护，Unix 使用仅属主可读文件；API 只返回 `has_key`，编辑留空保留原密钥。卸载保留配置和记录，配置包导入导出不包含通道。记录保留 30 天、最多 1000 条，界面读取最近 200 条。

公开原生动作：`channels.read/save/delete/default`、`notification.send`、`records.read/query`。通过已认证 `POST /api/extensions/gamer-notify/call` 调用，每次检查插件运行状态与权限。只有 notification.send 对 gamer-yaml 开放受控跨插件调用，不开放通道管理或任意网络请求。

独立构建：`./build.ps1`；主仓打包入口：`tools/build-plugins.ps1 -Plugin gamer-notify`。UI 联调：`node tools/build-plugin-ui.mjs gamer-notify`。

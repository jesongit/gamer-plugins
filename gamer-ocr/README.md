# OCR 历史插件（已迁入 Core）

OCR 已内置，无需安装或启用此插件。请在“设置 → 运行依赖”管理引擎和语言包，详见主仓 `docs/guides/runtime-dependencies.md`。旧私密配置、不可变安装与运行数据保留；下方仅为旧版文档，不再适用于新版安装。原有 YAML `ocr`、`find_text` 保持不变。Core MCP 使用 `ocr.read` / `ocr.recognize`，旧插件 scope 不自动获得新权限。

`gamer-ocr` 是可选 builtin 插件，使用本机 Tesseract 识别自动化截图中的文字。每次识别启动一个进程，结束即退出；多个账号共用一个识别队列，不创建每账号常驻 OCR 服务。插件归档仅包含声明，不携带程序、语言包或私有路径配置。

先安装 Tesseract 5 与 `tessdata_fast` 的 `chi_sim.traineddata`、`eng.traineddata`，在插件中心安装并启用“文字识别”。程序可放在 `server/deps/tesseract/tesseract.exe`，语言包放旁边 `tessdata/`；也支持系统 PATH 中的程序。插件设置中的路径留空自动检测，点击“检测可用性”检查程序及已安装语言包。Linux 使用 `tesseract` 与绝对路径的语言包目录。不会自动下载模型。

Windows 可显式运行下列命令安装固定版本的可选依赖（需要 7-Zip）：

```powershell
.\plugins\gamer-ocr\fetch-deps.ps1 -RuntimeDir D:\code\gamer\server\deps\tesseract
```

`runtime.lock.json` 固定 Tesseract 5.5.3.20260724 和 `tessdata_fast` 的提交、下载地址、SHA256 及模型大小。脚本校验下载后仅提取程序、DLL、版权文件和简体中文/英文模型，不运行安装程序；正式发行流程不会自动调用。已有同哈希文件直接复用，遇到不同文件会停止并要求另选目录，不删除或覆盖用户文件、其他语言包。下载临时目录保留用于检查，成功后会输出其路径。

路径配置保存在 `data/extension-data/gamer-ocr/private/settings.dat`，使用宿主私有配置保护，不进入配置包。可在“读取配置”查看当前路径；空路径保存表示恢复自动检测。程序不能来自已安装插件归档或配置包目录。

自动化应优先裁剪小区域再识别，复用 Core 原始截图，不使用多画面缩小预览。固定一行选 `line`，多行文字选 `block`，散布文字选 `sparse`；数字识别可设置 `whitelist: "0123456789"`。默认语言 `chi_sim+eng`、缩放 2 倍、总超时 10 秒，支持 1..3 倍及最长 15 秒。语言名称必须与已安装语言包匹配。

识别结果包含 `text`、0..1 的 `confidence`、`lines`、`words` 和输入图像 `width`、`height`。每个文字框包含 `text`、`confidence`、`x`、`y`、`width`、`height`，坐标始终对应输入 PNG 原始像素，内部放大不会改变坐标单位。未识别到文字返回空文字及置信度 0，不是运行异常。字体、描边和复杂背景会影响效果，置信度不等于准确率；实际排队页面需使用真实截图验收。

输入 PNG 上限 8 MiB、原图最多 400 万像素且边长不超过 4096，放大后最多 1600 万像素。共用队列最多接纳 16 个请求；只有当前请求解码和放大图像。超时包括排队、图像处理和识别。取消调用或停用插件会终止在途进程并等待退出，随后释放队列。

内部受权限与生命周期门禁保护的动作：

- `recognize`：`{png_base64, language?, mode?, whitelist?, scale?, timeout_ms?}`。
- `settings.read` / `settings.save`：读取及保存 `{engine_path, tessdata_path, expected_version?}`；API 带 `expected_version` 时校验并发修改。
- `status`：当前自动检测结果、可用语言、排队数和在途进程。

自动化函数的具体调用方式以宿主函数目录和 YAML V2 文档为准。识别本身不发送设备输入，之后点击仍需经过原有输入控制和帧校验。

识别使用 Tesseract 的 LSTM 模式及 TSV 输出参数，参考 [官方命令行说明](https://tesseract-ocr.github.io/tessdoc/Command-Line-Usage.html) 和 [官方 TSV 配置](https://github.com/tesseract-ocr/tesseract/blob/main/tessdata/configs/tsv)。

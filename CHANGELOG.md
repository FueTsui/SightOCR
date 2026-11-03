# 更新日志

记录 SightOCR 的用户可见变化。版本采用语义化版本号；更早的发布说明见 [GitHub Releases](https://github.com/FueTsui/SightOCR/releases)。

## [未发布]

### 文档

- 精简 GitHub 首页 README，补充接口与平台级别说明；新增 CLI JSON Schema，更新离线安装包中的文档和截图。

## [2.1.1] - 2026-11-03

### 新增

- 表格识别保留合并单元格：本地推断跨度，百度、腾讯采用接口返回的跨度。未经编辑的原文复制时同时提供 TSV 与 HTML；粘贴到 Excel、Word 可还原合并单元格。12 位及以上或以 0 开头的纯数字保持为文本，编辑后复制仍使用纯文本。
- 新增一次性命令行程序 `sightocr-cli.exe` 与配套 Skill，取代常驻 MCP 服务。新增 `providers` 命令；`--json` 输出 ASCII 转义的 JSON，`-o` 写入 UTF-8 文件；翻译原文可从 `--text`、`--file`（UTF-8 / UTF-16）或标准输入读取。
- 安装包检测到已安装版本时，可在窗口底部直接打开对应的卸载程序。

### 变更

- 升级时删除旧的 `sightocr-mcp.exe`；旧服务运行时，程序内更新会提示先停止服务。请从 AI 客户端删除旧 MCP 配置，并重新复制新版 Skill。
- 安装与卸载窗口的版本号移到名称下方居中显示；清单版本由 `Cargo.toml` 自动生成，EXE 版权声明与 MIT 许可一致。
- 缺少本地模型时，相关测试明确报告为忽略。打包脚本支持 `CARGO_TARGET_DIR`，校验清单不再使用 `../` 路径。

### 修复

- 修复识别结果滚动时部分文字行短暂缺字或消失的问题。原文与译文先在内存中绘制，再一次性显示；滚动条操作移到界面刷新完成后执行。
- 修复卸载完成页停留超过一分钟后，卸载程序要等系统重启才能删除的问题。
- 更新 CLI 的 UTF-16 解析实现，兼容新版 Rust 的严格 Clippy 检查，同时保留大小端、BOM 和非法输入校验。

### 安全

- MSVC 发布构建限制静态依赖从 System32 加载系统 DLL，降低安装包同目录 DLL 劫持风险；静态链接 C 运行库，无需另装 VC++ 运行库。

> 2.1.1 曾以相同版本号重新打包，修复识别结果滚动闪烁。已安装旧构建的 2.1.1 需要手动下载安装包覆盖安装；程序内更新不会提示同版本号的新构建。请用发布附件 `SHA256SUMS.txt` 区分文件。

## [2.1.0] - 2026-10-28

### 新增

- 原 Mistral、OpenAI、NVIDIA 固定接口统一为可自定义名称的「模型接口」，支持 OpenAI Chat Completions 兼容、OpenAI Responses、Anthropic Messages、Google Gemini、Mistral OCR 五种协议，用于文字、表格、公式识别与翻译；旧配置自动迁移。
- 设置页增加服务商快速填入、请求地址实时校验与连接测试。

### 改进

- 本地表格识别支持深色主题与斑马纹；中文、日文单元格换行不再插入多余空格。
- 优化按钮、切换、复制反馈和等待页等动效，并遵循 Windows「显示动画」设置。
- 改用 SightOCR 自带安装程序；覆盖安装保留设置、密钥与安装选项。

## [2.0.3] - 2026-09-24

### 修复

- 修复运行期间切换 Windows 系统代理后仍沿用旧连接的问题；新任务自动刷新代理与服务会话。

[未发布]: https://github.com/FueTsui/SightOCR/compare/v2.1.1...main
[2.1.1]: https://github.com/FueTsui/SightOCR/releases/tag/v2.1.1
[2.1.0]: https://github.com/FueTsui/SightOCR/releases/tag/v2.1.0
[2.0.3]: https://github.com/FueTsui/SightOCR/releases/tag/v2.0.3

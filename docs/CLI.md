# CLI 与 Skill

SightOCR 附带控制台程序 `sightocr-cli.exe`，供 AI agent 和脚本调用。每次调用只执行一条命令，完成后立即退出：不常驻后台、不监听网络端口，安装或更新时不会长期占用程序文件。2.1.0 及更早版本附带的 MCP 服务 `sightocr-mcp.exe` 已移除。

## 命令

```text
sightocr-cli ocr <图片> [--table | --layout] [--json] [-o <文件>]
sightocr-cli translate (--text <文本> | --file <文件> | 标准输入)
             [--from <代码>] [--to <代码>] [--provider <服务>] [--json] [-o <文件>]
sightocr-cli languages [--json]
sightocr-cli providers [--json]
sightocr-cli --help | --version
```

| 命令 | 用途 |
| --- | --- |
| `ocr` | 本地识别 PNG/JPEG/BMP/WebP 图片，不上传图片。`--table` 输出 TSV（单元格以制表符分隔，每行一条记录）；`--layout` 按阅读顺序输出，合并因宽度换行的行，每段一行。两者不能同时使用 |
| `translate` | 把文字发给已保存或 `--provider` 指定的翻译服务，会访问网络。`--from`、`--to` 取自 `languages`，只有 `--from` 可为 `auto`；`--provider` 为 `bing`、`baidu`、`tencent` 或 `model:<模型接口编号>`（旧名称 `openai`、`nvidia`、`generic` 仍可用）。语言和服务覆盖只对本次调用生效 |
| `languages` | 列出语言代码 |
| `providers` | 列出可用的翻译服务、是否已配置、默认服务和默认语言，不输出密钥 |

翻译原文三选一：短文本用 `--text`；长文本或多行文本用 `--file`（UTF-8，或带 BOM 的 UTF-16，即 Windows PowerShell 5.1 `Out-File` 的默认格式）；也可从标准输入读取。

## 输出与退出码

- 默认把结果原文写到 stdout，不做转义；`languages` 和 `providers` 每项一行，字段以制表符分隔。
- `--json` 输出一个 JSON 对象，所有非 ASCII 字符写成 `\uXXXX`，在任何控制台代码页下都不会乱码。识别和翻译结果在 `text` 字段。
- `-o <文件>` 把结果以 UTF-8（无 BOM）写入文件，stdout 不输出内容。
- 错误信息写到 stderr；同时指定 `--json` 时，stdout 另输出 `{"error": "..."}`。
- 退出码：`0` 成功，`1` 执行失败（文件不存在、识别失败、网络或服务错误等），`2` 用法错误。

在中文 Windows 上，Windows PowerShell 5.1 默认按 GBK 解码外部程序的输出，并按 ASCII 向外部程序的标准输入发送文本。因此在 PowerShell 5.1 中读取中文结果请用 `--json` 或 `-o`，传入中文请用 `--text` 或 `--file`，不要用管道 `|`。PowerShell 7 与 cmd 不受影响。

## 配置与限制

CLI 只读取配置，不迁移旧配置、不保存语言或服务覆盖项，也不输出密钥。默认读取 `%APPDATA%\SightOCR\config.json`，设置 `SIGHTOCR_CONFIG` 后读取该文件；没有配置文件时使用默认值（Bing、自动检测、简体中文）。模型目录默认为程序旁的 `resources\oneocr`，可用 `SIGHTOCR_RESOURCES` 指定。桌面设置中保存的服务凭据和代理对之后的调用生效。

图片上限为 64 MiB / 1 亿像素，文字上限为 1 MiB，单次处理上限为 120 秒。每次调用都要重新加载 OneOCR 模型，识别一张图片通常需要约 1 秒；安装或更新后的首次调用需要读取模型文件，安全软件也可能扫描新程序，可能需要 10 秒左右。

## Skill

安装目录中的 `skills\sightocr\SKILL.md` 是配套的通用 Skill，供支持 Skill 的 AI 客户端使用。安装器不会自动复制它。请将整个 `sightocr` 文件夹复制到客户端的技能目录，例如：

| 客户端 | 用户级技能目录 |
| --- | --- |
| Claude Code | `%USERPROFILE%\.claude\skills\` |
| Codex | `%USERPROFILE%\.agents\skills\` |

其他客户端请参考其文档。Skill 会依次在 `PATH` 和默认安装路径 `C:\Program Files\SightOCR\sightocr-cli.exe` 中查找 CLI。安装到其他目录时，请将该目录加入 `PATH`，或修改 `SKILL.md` 中的路径。客户端可能会对每条命令请求执行授权，可在客户端中将 `sightocr-cli.exe` 设为允许执行。升级 SightOCR 后，如需使用新版 Skill，请重新复制。

例如：“用 SightOCR 识别这张本地图片中的表格”或“识别图片并翻译为中文”。

## 从 MCP 迁移

1. 从 AI 客户端的 MCP 配置中删除 `sightocr`（`sightocr-mcp.exe`）服务。
2. 安装新版。安装器会删除旧的 `sightocr-mcp.exe` 和 `docs\MCP.md`。程序内更新时，若旧 MCP 服务仍在运行，更新会停止并提示先停止服务，不会替换任何文件。
3. 复制新版 Skill，替换客户端技能目录中的旧版 `sightocr` 文件夹。

## 排错

- OneOCR 加载失败：检查程序旁 `resources\oneocr` 是否同时存在 `oneocr.dll`、`onnxruntime.dll`、`oneocr.onemodel`，或设置 `SIGHTOCR_RESOURCES`。
- 中文结果显示为乱码：改用 `--json` 或 `-o`，见上文“输出与退出码”。
- Bing 授权失败：新版允许授权页在可信 Bing HTTPS 站点间进行地区跳转；网络阻断、服务页面变化或额度问题仍可能导致请求失败。检查桌面设置中的代理后重试。
- 指定的模型接口不存在或不能翻译：运行 `sightocr-cli providers` 查看可用的编号。

## 开发验证

常规回归随 `cargo test --locked` 运行，其中进程集成测试会通过真实管道检查中文输入、UTF-16 文件、JSON 输出、退出码和配置保持；仓库带有 OneOCR 模型时还会识别示例表格。安装回归（PowerShell 7）在隔离环境中安装 `dist/SightOCR`，验证旧 MCP 服务运行时拒绝更新、升级时清理旧文件，并用安装后的 CLI 与真实模型识别表格：

```powershell
./scripts/test-cli.ps1
pwsh -NoProfile -File ./scripts/test-installer.ps1
```

这些测试只使用合成图片和同语言翻译，不向在线翻译服务发送请求。

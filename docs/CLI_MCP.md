# CLI 与 MCP

安装包和便携目录包含两个程序：`SightOCR.exe` 启动桌面界面，`sightocr-cli.exe` 用于命令行和本地 MCP。两者复用 Rust OCR、翻译及代理实现，不需要 Python、Node.js 或管理员权限。下面命令在便携目录运行；安装版可用 `%LOCALAPPDATA%\Programs\SightOCR\sightocr-cli.exe` 的完整路径。

## 命令行

```powershell
.\sightocr-cli.exe --help
.\sightocr-cli.exe --version
.\sightocr-cli.exe languages --format json

# 本地文字识别；图片路径有空格时加引号
.\sightocr-cli.exe ocr 'C:\Images\example.png'
.\sightocr-cli.exe ocr 'C:\Images\table.png' --table --output .\table.tsv
.\sightocr-cli.exe ocr 'C:\Images\example.png' --format json

# 指定服务只影响本次调用
.\sightocr-cli.exe translate 'Hello world' --from en --to zh-Hans --provider bing
.\sightocr-cli.exe translate --input .\source.txt --to en --format json --output .\translated.json

# Windows PowerShell 5.1 通过管道发送中文时须显式使用 UTF-8
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
'你好，世界' | .\sightocr-cli.exe translate --stdin --from zh-Hans --to en
```

翻译输入在直接文本、`--input FILE`、`--stdin` 三种方式中选择一种。输入文件采用 UTF-8；输出采用 UTF-8，默认文本，可用 `--format json` 获取结构化结果。成功返回码为 `0`，失败返回非 `0`，错误写入 stderr；`--output` 会写入指定文件并替换已有文件。自动化调用应检查退出码。

`--provider` 可选 `bing`、`baidu`、`tencent`、`openai`、`nvidia`；省略时使用已保存的翻译服务。`--from` 和 `--to` 使用 `languages` 列出的语言代码。OCR 始终使用本地 OneOCR，表格返回 TSV；翻译会将文本发送到所选服务，相同源语言和目标语言直接返回原文。

兼容旧参数 `--ocr IMAGE`、`--translate TEXT`；桌面程序继续接受旧命令。脚本和 MCP 使用控制台程序以可靠等待进程并读取标准输入输出。

## MCP 配置

MCP 通过标准输入输出运行，不监听网络端口，不启动桌面窗口或托盘。将以下示例加入支持 stdio MCP 的客户端配置，替换 `command` 为实际绝对路径：

```json
{
  "mcpServers": {
    "sightocr": {
      "command": "C:\\Users\\YOUR_USER\\AppData\\Local\\Programs\\SightOCR\\sightocr-cli.exe",
      "args": ["mcp"]
    }
  }
}
```

客户端需要独立配置或模型位置时，在该服务的 `env` 中指定：

```json
{
  "SIGHTOCR_CONFIG": "C:\\SightOCR\\config.json",
  "SIGHTOCR_RESOURCES": "C:\\SightOCR\\resources\\oneocr"
}
```

| 工具 | 参数 | 用途 |
| --- | --- | --- |
| `sightocr_ocr` | `image_path`，可选 `table` | 读取本机 PNG/JPEG/BMP/WebP 并本地识别；不上传图片 |
| `sightocr_translate` | `text`，可选 `source_lang`、`target_lang`、`provider` | 按所选服务翻译文本；会访问网络 |
| `sightocr_languages` | 空对象 | 列出可选语言与代码 |

工具发现提供输入及输出 schema、只读/网络访问提示。调用结果提供文本内容和 `structuredContent`。stdout 只传输 JSON-RPC 消息，诊断写入 stderr。当前服务串行处理调用，单个后台任务上限为 120 秒；取消通知不会中断正在执行的任务，关闭 stdin 会在当前调用结束后退出。需要立即停止时由客户端结束服务进程，其后台进程随 Windows Job 一起回收。

协议依据：[stdio 传输](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)、[初始化与版本协商](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)、[工具调用](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)。

## 配置与排错

CLI/MCP 只读取配置，不迁移旧配置、不保存服务或语言覆盖项，也不输出密钥。默认读取 `%APPDATA%\SightOCR\config.json`；设置 `SIGHTOCR_CONFIG` 后读取该文件。没有配置文件时使用默认值。桌面设置中保存的服务凭据和代理对后续命令生效。

- OneOCR 加载失败：检查程序旁 `resources\oneocr` 是否同时存在 `oneocr.dll`、`onnxruntime.dll`、`oneocr.onemodel`，或设置 `SIGHTOCR_RESOURCES`。
- 中文输入乱码：优先用 UTF-8 文件配合 `--input`；Windows PowerShell 5.1 管道应设置 `$OutputEncoding`。
- MCP 启动后没有普通提示文本：属于正常行为，客户端需发送初始化请求；用 `--help` 单独检查程序能否运行。
- Bing 授权失败：新版允许授权页在可信 Bing HTTPS 站点间进行地区跳转；网络阻断、服务页面变化或额度问题仍可能导致请求失败。检查桌面代理设置后重试。
- 更新或卸载前关闭 MCP 客户端启动的 SightOCR 服务，避免控制台程序占用安装文件。安装器会检测同一安装目录的 GUI 和 CLI 进程；普通静默安装不会擅自结束它们。自动更新检测到 CLI/MCP 在运行时会立即提示先停止服务，不会等待 120 秒或替换文件。

## 开发验证

常规回归随 `cargo test --locked` 运行。另可使用官方 MCP Python 客户端检查真实发布程序的 stdio、工具 schema、中文输入、本地 OCR 和配置保持：

```powershell
# 仅开发验证需要这些依赖，使用程序不需要 Python
python -m pip install mcp jsonschema
python scripts/test-cli-mcp.py --binary dist/SightOCR/sightocr-cli.exe --report target/cli-mcp-smoke.json
```

该脚本只使用合成图片和同语言翻译，不向在线翻译服务发送请求。

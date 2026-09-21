# MCP 与 Skill

SightOCR 提供专用 `sightocr-mcp.exe`，直接启动 stdio MCP，无需参数。已移除 CLI 命令和桌面程序的旧命令行 OCR/翻译入口。

## MCP 配置

MCP 通过标准输入输出运行，不监听网络端口，不启动桌面窗口或托盘。将以下示例加入支持 stdio MCP 的客户端配置，替换 `command` 为实际绝对路径：

```json
{
  "mcpServers": {
    "sightocr": {
      "command": "C:\\Users\\YOUR_USER\\AppData\\Local\\Programs\\SightOCR\\sightocr-mcp.exe",
      "args": []
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

MCP 只读取配置，不迁移旧配置、不保存服务或语言覆盖项，也不输出密钥。默认读取 `%APPDATA%\SightOCR\config.json`；设置 `SIGHTOCR_CONFIG` 后读取该文件。没有配置文件时使用默认值。桌面设置中保存的服务凭据和代理对后续命令生效。

- OneOCR 加载失败：检查程序旁 `resources\oneocr` 是否同时存在 `oneocr.dll`、`onnxruntime.dll`、`oneocr.onemodel`，或设置 `SIGHTOCR_RESOURCES`。
- Bing 授权失败：新版允许授权页在可信 Bing HTTPS 站点间进行地区跳转；网络阻断、服务页面变化或额度问题仍可能导致请求失败。检查桌面代理设置后重试。
- 更新或卸载前关闭 MCP 客户端启动的 SightOCR 服务，避免控制台程序占用安装文件。安装器会检测同一安装目录的 GUI 和 MCP 进程；普通静默安装不会擅自结束它们。自动更新检测到 MCP 在运行时会立即提示先停止服务，不会等待 120 秒或替换文件。

## 开发验证

常规回归随 `cargo test --locked` 运行。使用 Rust 进程集成测试检查 stdio、中文输入、工具错误恢复与配置保持；安装回归使用真实模型验证本地表格 OCR：

```powershell
./scripts/test-mcp.ps1
./scripts/test-installer.ps1
```

这些测试只使用合成图片和同语言翻译，不向在线翻译服务发送请求，不需要 Python。

## 通用 Skill 调用

安装目录的 `skills/sightocr/SKILL.md` 是面向支持 Skill 与 MCP 的 AI 客户端的通用技能。将 `sightocr` 文件夹复制到客户端支持的技能目录，并按上文配置 MCP 服务；Skill 不会自动注册服务，也不依赖某个特定模型或运行器。客户端载入 Skill 后发现实际工具名，再调用识别、语言查询和翻译工具。工具名可能带客户端添加的服务前缀。

例如：“用 SightOCR 识别这张本地图片中的表格”或“识别图片并翻译为中文”。Skill 会依次调用本地 OCR 和文本翻译，保留换行/表格结构，并检查工具结果。MCP 服务本身不解释 SKILL.md 或执行任意外部脚本。

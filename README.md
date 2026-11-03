<div align="center">

<img src="assets/icon.png" width="88" alt="SightOCR 图标">

# SightOCR

Windows 截图识别与翻译工具：框选屏幕，提取文字、表格或公式，按需翻译并复制结果。

[![Release](https://img.shields.io/github/v/release/FueTsui/SightOCR?label=%E6%9C%80%E6%96%B0%E7%89%88%E6%9C%AC)](https://github.com/FueTsui/SightOCR/releases/latest)
[![CI](https://github.com/FueTsui/SightOCR/actions/workflows/rust.yml/badge.svg)](https://github.com/FueTsui/SightOCR/actions/workflows/rust.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[下载](https://github.com/FueTsui/SightOCR/releases/latest) · [快速上手](#快速上手) · [CLI 与 Skill](docs/CLI.md) · [完整文档](#文档)

</div>

<p align="center">
  <img src="docs/screenshots/2.1.0/result.png" width="70%" alt="SightOCR 识别结果界面，展示多语言文字识别结果">
</p>

> 界面截图采集于 2.1.0。当前源码与最新公开版本均为 **2.1.1**；安装包、便携包和校验文件见 [v2.1.1 发布页](https://github.com/FueTsui/SightOCR/releases/tag/v2.1.1)。

## 功能

- **截图即识别**：支持文字、表格与公式；普通识别、截图翻译和静默识别都有全局快捷键。
- **本地 OCR**：默认使用 OneOCR 在本机识别文字与表格，支持段落、双栏版面和多种表格样式；本地引擎不支持公式。
- **可编辑的结果**：原文与译文可以编辑、复制；未编辑的表格结果同时提供 TSV 和 HTML，粘贴到 Excel、Word 可保留合并单元格与长数字文本格式。
- **可选的云端服务**：百度、腾讯及五种模型接口协议可用于识别或翻译；Bing 可用于免密钥翻译。使用云端服务时会向所选服务发送相应图片或文字。
- **脚本与 AI 调用**：`sightocr-cli.exe` 每次执行一条命令后退出；安装包附带可手动复制到 AI 客户端的 Skill。

详细行为与适用范围见 [使用说明](docs/USAGE.md)。

## 下载与安装

从 [最新发布页](https://github.com/FueTsui/SightOCR/releases/latest) 选择：

| 文件 | 用途 |
| --- | --- |
| `SightOCR-Setup-2.1.1.exe` | 安装或覆盖升级；安装时需要 Windows 管理员授权 |
| `SightOCR-2.1.1-portable.zip` | 解压后运行 `SightOCR/SightOCR.exe` |
| `SHA256SUMS.txt` | 核对下载文件的 SHA-256 |

安装包目前**未配置代码签名**。覆盖安装会保留设置；若已安装旧构建的 2.1.1，须手动下载并运行安装包，程序内更新不会提示相同版本号的新构建。安装选项、升级与卸载见 [安装说明](docs/INSTALLATION.md)。

## 快速上手

1. 启动 SightOCR；程序会驻留在系统托盘。
2. 按 <kbd>F5</kbd> 并框选屏幕区域，查看和复制识别结果。
3. 需要翻译时按 <kbd>F2</kbd>；只想把识别文字复制到剪贴板时按 <kbd>F4</kbd>。

| 默认快捷键 | 操作 |
| --- | --- |
| <kbd>F5</kbd> | 截图识别，显示并复制原文 |
| <kbd>F2</kbd> | 截图识别并翻译，复制译文 |
| <kbd>F4</kbd> | 静默识别，直接复制原文 |
| <kbd>Esc</kbd> / 鼠标右键 | 取消框选 |

快捷键可在「设置 → 快捷键」中修改。关闭主窗口会隐藏到托盘；要结束程序，请从托盘菜单选择「退出」。

## 接口

### 能力概览

| 能力 | 桌面入口 | SDK 函数 | CLI 命令 | MCP 工具 | HTTP 端点 | 副作用级别 |
| --- | --- | --- | --- | --- | --- | --- |
| 截图文字 / 表格识别 | <kbd>F5</kbd>、<kbd>F4</kbd> | — | `ocr <图片> [--table \| --layout]`（识别本地文件） | — | — | 本地写入（桌面端剪贴板） |
| 公式识别 | 工具栏选择公式与云端引擎 | — | — | — | — | 出站（图片）及本地写入 |
| 文字翻译 | <kbd>F2</kbd>、翻译原文 | — | `translate` | — | — | 出站（文字）及本地写入 |
| 查询语言 / 服务 | 设置页 | — | `languages`、`providers` | — | — | 只读 |

### SDK

当前没有对外承诺稳定的 SDK。

### CLI

`sightocr-cli.exe` 读取本地图片或文字，一次运行处理一条命令。`--json` 输出一个 JSON 对象，结构见 [CLI JSON Schema](docs/CLI.schema.json)；成功退出码为 `0`，执行失败为 `1`，参数错误为 `2`，处理任务最多等待 120 秒。完整参数、纯文本输出和 Skill 安装方法见 [CLI 与 Skill](docs/CLI.md)。例如：

```powershell
sightocr-cli ocr 'C:\Pictures\table.png' --table --json
sightocr-cli translate --text 'Hello' --to zh-Hans
sightocr-cli languages
```

### MCP

2.1.1 已移除旧版 `sightocr-mcp.exe`，不提供 MCP 工具。AI 客户端可通过配套 Skill 调用 CLI。

### HTTP API

当前不提供 HTTP 服务或端点。

### 配置与凭据

桌面端在「设置 → 接口」配置云端服务和模型接口，在「设置 → 代理」配置网络代理。设置保存在 `%APPDATA%\SightOCR\config.json`；CLI 默认读取同一配置，也可分别用 `SIGHTOCR_CONFIG` 和 `SIGHTOCR_RESOURCES` 指定配置文件与 OneOCR 资源目录。服务密钥在本机配置文件中以明文保存，界面仅遮罩显示，请勿分享该文件。

### 兼容性与平台级别

| 平台 | 级别 | 验证范围 |
| --- | --- | --- |
| Windows 11 x64 | Tier 1，支持 | 本机 GUI / CLI 验证与 Windows CI；发布包未签名 |
| Windows 10 22H2 x64 | Tier 2，预览 | 使用同一 x64 构建；当前版本未在 Win10 实机复测 |
| Windows Arm64（x64 模拟） | 未支持 | 尚未验证 |
| macOS、Linux | 未支持 | 没有对应构建 |

具体测试结果和未覆盖场景见 [验证记录](docs/VALIDATION.md)。

## 从源码构建

需要 Rust 1.88+ 的 MSVC 工具链、Visual Studio C++ Build Tools 与 Windows SDK。公开源码不包含 OneOCR DLL 和模型；可用 `tests/ExtractOCR.bat` 从本机 Windows 截图工具提取资源，此步骤需要管理员权限。

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked -- --test-threads=1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1 -Installer
```

缺少 OneOCR 资源时，依赖模型的测试会显示为“忽略”，不计为通过。打包与验证细节见 [安装说明](docs/INSTALLATION.md)和 [验证记录](docs/VALIDATION.md)。

## 文档

| 文档 | 内容 |
| --- | --- |
| [使用说明](docs/USAGE.md) | 桌面功能、识别引擎与翻译服务 |
| [安装说明](docs/INSTALLATION.md) | 安装、升级、程序内更新与卸载 |
| [CLI 与 Skill](docs/CLI.md) | 命令、输出、Skill 安装与排错 |
| [更新日志](CHANGELOG.md) | 各版本的用户可见变化 |
| [验证记录](docs/VALIDATION.md) | 实测结果与未覆盖场景 |
| [界面设计记录](docs/UI_DESIGN.md) | 视觉规范与验证范围 |
| [Rust 重构记录](docs/RUST_REFACTOR.md) | 项目结构与人工回归项 |

## 限制与许可

复杂合并单元格、强透视或模糊照片需人工校对；本地引擎不支持公式识别。多屏混合 DPI、远程桌面与真实云端账号组合尚未完成全面验证。

SightOCR 第一方代码采用 [MIT 许可](LICENSE)。OneOCR DLL、模型及其他第三方组件保留各自的版权与许可。

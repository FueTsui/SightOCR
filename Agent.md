# Agent.md

本文件给在 SightOCR 仓库中工作的 AI agent（Claude Code、Codex 等）提供项目上下文与约定。面向用户的说明见 [README.md](README.md) 与 `docs/`。

## 项目概览

SightOCR 是 Windows 10 / 11 x64 上的截图识别与翻译工具，纯 Rust 实现（egui + Win32），无 Python / Qt 运行时。

- 识别类型：文字、表格（输出 TSV）、公式（输出 LaTeX）
- 本地引擎：OneOCR（来自 Windows 截图工具的 DLL 与模型），离线运行
- 云端：百度、腾讯，以及「模型接口」（OpenAI Chat / Responses、Anthropic Messages、Gemini、Mistral OCR 五种协议）
- 翻译：Bing（默认，免密钥）、百度、腾讯、模型接口
- 附带 `sightocr-cli.exe` 与 `skills/sightocr/SKILL.md`，供 AI agent 一次性调用（旧 MCP 服务已在 2.1.1 移除）

当前版本以 `Cargo.toml` 的 `version` 为准；最低 Rust 版本 1.88，edition 2021。

## 目录结构

```text
rust/
├── lib.rs             库入口（cli / config / ocr / platform / services / updater / worker）
├── main.rs            GUI 入口，bin: SightOCR
├── console.rs         CLI 入口，bin: sightocr-cli
├── setup/             自带安装程序，bin: SightOCR-Setup
├── app.rs             应用状态、任务与系统事件
├── config.rs          配置验证、迁移与原子保存
├── headless.rs        无界面识别与翻译
├── ocr.rs, ocr/       OneOCR FFI、COM 生命周期、表格恢复与版面分析
├── platform.rs, platform/  截图、热键、托盘、自启、单实例
├── services.rs, services/  云端 OCR、翻译、签名、代理；services/ai.rs 为模型接口
├── ui/                工作台、设置、主题、原生 RichEdit 文本控件、各类 smoke 测试
├── updater.rs         程序内更新
└── worker.rs, worker/ 有界队列、图像加载、识别子进程调度
tests/                 集成测试（cli.rs、live_models.rs）与公开夹具
scripts/               build / run / 回归测试 PowerShell 脚本
skills/sightocr/       随包发布的通用 Skill
docs/                  使用、安装、CLI、UI 设计、重构与验证记录
resources/oneocr/      OneOCR DLL 与模型（不随源码提供，已 gitignore）
```

以下目录是生成物，不要手动编辑或依据它们判断源码：`target/`、`dist/`、`release/`。

## 常用命令

在仓库根目录用 PowerShell 执行：

```powershell
cargo run --locked --bin SightOCR                       # 开发运行 GUI
cargo fmt --all -- --check                              # 格式检查
cargo clippy --locked --all-targets -- -D warnings      # 严格 Clippy
cargo test --locked -- --test-threads=1                 # 默认测试（必须串行）
cargo build --release --locked --bins                   # 发布构建
```

工具链：日常开发可用任意 Windows 工具链，但发布构建必须用 MSVC（`build.ps1 -Toolchain <msvc 工具链>`，默认 `stable-x86_64-pc-windows-msvc`）。原因：`build.rs` 只在 MSVC 下加 `/DEPENDENTLOADFLAG:0x800`，`.cargo/config.toml` 为 MSVC 静态链接 CRT；`tests/pe_hardening.rs` 在非 MSVC 工具链下显示为忽略。`rust/app.manifest` 的 `@VERSION@` 由 `build.rs` 按 `Cargo.toml` 版本填入，不要手写版本号。

打包：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1             # 检查 + 测试 + 构建 + 复制到 dist/SightOCR
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1 -Installer  # 额外生成安装包
```

其他回归：`scripts/test-cli.ps1`、`scripts/test-installer.ps1`（需 PowerShell 7）、`scripts/test-updater.ps1`、`scripts/test-table-samples.ps1`。

特殊测试：

- `cargo test --locked --lib -- --ignored --test-threads=1`：依赖补充字体或交互桌面的测试，默认忽略。
- `cargo test --locked --features private-table-samples -- --test-threads=1`：需要本地私有表格夹具，公开仓库没有。
- `cargo test --test live_models -- --ignored --nocapture`：读取根目录 `.env`，调用真实云端服务并**消耗配额**。只在用户明确要求时运行。
- `cargo run --locked --bin SightOCR -- --smoke-ui <输出目录>`：仅 Debug 构建可用，隔离配置、不碰剪贴板和网络，输出截图与 JSON 报告。

## 提交前检查

CI（`.github/workflows/rust.yml`，windows-latest）依次运行 fmt、clippy、test、release build。改完代码后至少本地跑通前三项：

1. `cargo fmt --all -- --check`
2. `cargo clippy --locked --all-targets -- -D warnings`（任何警告都会失败）
3. `cargo test --locked -- --test-threads=1`

测试必须加 `--test-threads=1`：原生字体、焦点与 RichEdit 测试共享 Windows 桌面状态，并行会互相干扰。

## 代码约定

- **unsafe**：Clippy 开启了 `undocumented_unsafe_blocks`，每个 `unsafe` 块前都要写 `// SAFETY:` 注释说明为什么安全。
- **线程模型**（详见 `docs/RUST_REFACTOR.md`）：
  - UI 线程拥有配置、egui 界面和原生 RichEdit 控件。
  - OneOCR、COM apartment 和 HTTP 缓存由一个持久的私有识别子进程独占，通过匿名管道传递任务；COM 对象在同一线程创建、使用和销毁，不实现 `Send`。
  - 任务带编号，最多一个待处理任务；取消时终止子进程，迟到的结果按编号丢弃。
  - 截图前要等主窗口隐藏后的新渲染帧提交再捕获；egui 同一帧的多次布局 pass 不算新帧。
- **错误处理**：用 `Result` / `anyhow` 传播，不要把错误字符串混入识别或翻译结果；翻译失败的信息不能当作译文复制。
- **配置**：保存在 `%APPDATA%`，原子写入；新增字段要兼容旧配置迁移。CLI 只读配置，不迁移、不创建、不保存。
- **隐私**：截图只在内存中处理，凭据和图片不写入命令行参数或临时文件。
- **依赖**：`eframe` 锁定在 `=0.31.1`，`reqwest` 只用 rustls；升级依赖前先确认原因。
- **注释与文档**：代码注释用英文，`README.md` 与 `docs/` 用简体中文，与现有风格保持一致。

## 敏感文件

以下文件含密钥、私有数据或第三方二进制，已在 `.gitignore` 中。不要读取后输出其中的值，不要提交，也不要复制到 `dist/` 以外的位置：

- `.env`、`.env.*`：真实云端服务密钥（仅 `live_models` 测试使用）
- `config.json`：本机配置，可能含服务密钥
- `resources/oneocr/`、`tests/oneocr/`：OneOCR DLL 与模型（可用 `tests/ExtractOCR.bat` 从本机提取，需管理员权限）
- `tests/样本/`、`tests/fixtures/table-lines/`：私有业务表格及派生夹具

## 测试与验证记录

- 不要对用户正在运行的 SightOCR 实例发送窗口消息、按键或剪贴板操作；交互测试使用测试进程自己创建的窗口。
- 代理相关测试通过 `RegOverridePredefKey` 重定向注册表，不修改用户的真实系统代理。
- 在 `docs/VALIDATION.md` 写验证记录时如实区分「通过」「忽略」「未覆盖」：被忽略或跳过的测试不能计为通过，使用 `-SkipOptionalFontTests` 时必须注明。
- 不要把依赖减少表述为已实测的内存或速度提升，除非确实做了基准测试。

## 发布

1. 更新 `Cargo.toml` 版本号与 `README.md` 更新日志。
2. 运行 `scripts/build.ps1 -Installer`，产物在 `dist/SightOCR` 与 `dist/installer/`。
3. 发布附件包含安装包、便携 ZIP、`SHA256SUMS.txt` 与 `VALIDATION.json`。
4. 打包只附带用户文档（`USAGE.md`、`INSTALLATION.md`、`CLI.md`），开发记录留在仓库。

发布、推送、打 tag 等对外操作需先征得用户同意。

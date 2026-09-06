# SightOCR

Windows 截图识别与翻译工具，现采用 **Rust + egui + Win32** 重写，运行时不需要 Python、Qt、NumPy 或 OpenCV。

## 运行

运行 `dist\installer\SightOCR-Setup-2.0.0.exe` 安装；也可双击 `启动.bat`，或运行便携目录中的 `dist\SightOCR\SightOCR.exe`。本次多语言排版与即时取消修复按要求沿用 2.0.0 版本号，请通过 SHA256 区分同版本历史构建。从源码启动且尚未构建时，脚本会调用 Cargo 构建 Release。要求 Windows 10/11 x64。安装、升级和卸载说明见 [Windows 安装包](docs/INSTALLATION.md)。

| 操作 | 功能 |
| --- | --- |
| F4（默认） | 静默框选识别并复制原文，不弹出结果窗口 |
| F5（默认） | 框选识别，显示结果并复制原文 |
| F2（默认） | 框选识别、翻译并复制译文 |
| Esc / 鼠标右键 | 取消截图 |
| 界面「输入文字」 | 编辑原文并提交翻译 |
| 关闭窗口 | 隐藏窗口并继续在后台运行，与托盘图标显示或系统集成服务是否可用无关 |
| 工具栏图钉 / 齿轮 | 切换窗口置顶 / 直接打开设置 |
| 托盘菜单「退出」 | 退出并释放资源 |

支持自定义快捷键、接口与语言选择、窗口置顶、主题切换、用户级开机自启。无须管理员权限。原文框可编辑换行，点击「翻译原文」提交。处理期间不会重复排队；取消后立即恢复操作，保留已显示的文本，可马上开始新任务。后台识别进程及其网络连接会被终止，迟到结果不会覆盖新任务。

修改快捷键时，在「设置 → 快捷键」按顺序录入静默识别、截图识别、截图翻译三项快捷键。点击对应控件后直接按下单键或组合键，无需手动输入 `Ctrl+F4` 等文字。三项不能使用相同的按键组合；录入结果先保留在设置草稿中，保存成功后才生效，取消设置或保存失败时保持原快捷键。

## 桌面界面

界面参考 Windows 11 截图工具：顶部紧凑工具栏、蓝色模式指示、浅色画布和居中快捷键提示。识别完成后先展示原文，继续翻译时在译文区域显示等待状态。保留系统标题栏、拖动、调整大小、最小化和最大化；标题栏随浅色/深色主题同步，也可选择跟随系统。原文与译文使用 Windows 原生 Unicode 编辑控件，支持多语言字体回退、阿拉伯文/波斯文等双向排版、印度及东南亚文字连写和输入法；界面字体同时补齐各可选语言的系统字体回退。复制和翻译使用原始字符顺序。

- **工具栏**：「新建」开始截图，旁边切换截图识别/截图并翻译模式；选择文字、表格、公式类型及本地/云端引擎。右侧图钉切换窗口置顶，齿轮直接打开设置；识别精度保留在引擎选择中。
- **原文 / 译文**：默认宽度使用紧凑分页，窗口加宽后可左右对照。语言选择、复制和翻译操作放在对应区域，文本可独立滚动，底部操作保持可见。
- **设置**：按顺序包含常规、接口、快捷键、代理、关于五页，「返回」与导航位于同一行。常规直接展示应用主题、合并文本换行、开机自动启动和隐藏托盘图标，移除分组标题；接口通过七项服务选择器切换凭据与模型表单；快捷键独立成页，直接展示三项录入控件，位于接口与代理之间；关于页提供版本和配置位置，移除产品简介与自动更新流程说明，退出操作位于托盘菜单。语言与翻译服务在工作台选择。底部固定「保存」与「取消」按钮，外观与快捷键在保存成功后生效。
- **任务反馈**：普通识别与翻译的等待页显示当前阶段、提交任务时选定的服务与目标语言、已用时间和取消入口。截图并翻译会先展示识别原文，再等待译文。取消后忽略迟到的进度与完成事件；处理期间锁定输入，避免结果覆盖正在编辑的文字。

桌面界面专注截图与文字输入，已移除图片导入和拖放识别。文件识别仍可使用命令行 `--ocr`，支持 PNG、JPEG、BMP、WebP。

托盘悬停提示仅显示「SightOCR」。托盘菜单按顺序包含「主界面、静默识别、截图识别、截图翻译、设置、重启、退出」七项。三项截图操作旁显示当前生效的快捷键；设置保存成功后同步更新，保存失败时保留原快捷键。重启会先释放窗口、任务与热键，再启动新进程。

「隐藏托盘图标」仅控制图标显示。关闭主窗口始终隐藏到后台，即使系统集成服务初始化失败也不退出；服务可用时全局快捷键继续响应，`--silent` 启动同样保持隐藏。正常情况下再次运行 SightOCR 可打开已有实例。托盘「退出」明确结束进程，「重启」结束当前进程后重新启动；这些动作与关闭窗口分别处理。

普通识别完成时恢复并将结果窗口移至普通窗口最前方，保留用户调整的尺寸、最大化状态和置顶选择；静默识别不自动打开窗口。设置页已移除冗余说明，「获取密钥」直接打开对应官方控制台。设置滚动条使用圆角滑块、明显的浅深主题对比与悬停展开，滚动区保留固定边距。

主窗口客户区默认 740×680、最小 740×580 逻辑像素，可调整大小和最大化，未设置最大尺寸限制。默认尺寸在 150% 缩放下对应 1110×1020 物理像素。初始位置在当前显示器工作区居中，避开任务栏占用区域。视觉规范和验证范围见 [界面设计记录](docs/UI_DESIGN.md)。

程序/托盘图标使用 `assets/icon.ico`，窗口与关于页使用 `assets/icon.png`，截图光标使用 `assets/cursor/cross.png`。资源直接嵌入程序，发布后无需另外复制 assets 目录。

工具栏、全局快捷键和托盘菜单发起截图时，可见的主窗口先最小化并隐藏，再进入框选；已隐藏到托盘的窗口保持隐藏，直接进入框选。普通识别和截图并翻译在框选完成后恢复主窗口，显示等待页与结果。

静默识别仍显示框选覆盖层，成功后自动复制原文，不进行翻译，也不自动弹出等待页或文字结果窗口。进度、取消与失败均不自动恢复主窗口；可通过托盘「主界面」或「设置」主动打开。静默仅作用于本次请求，下一次普通识别或翻译仍正常显示界面。启动参数 `--silent` 则表示启动后收起到托盘，与单次静默识别分别控制。

框选样式沿用旧版：1px 纯白边框，选区外保持原图一半亮度，选区内显示清晰原图。预览在离屏缓冲中完成合成，再一次提交到窗口，避免先显示暗层、后显示选区造成的闪烁；原始截图保持只读。

默认本地 OneOCR，截图只在内存中处理。可选百度、腾讯、Mistral、OpenAI、NVIDIA 进行云端文字、表格或公式识别。本地表格输出 TSV，可粘贴到 Excel。云端 OCR 失败时显示原因并回退本地，保留接口选择；公式回退只能获得普通文字。

百度、腾讯公式识别结果采用与 Mistral 示例一致的 LaTeX 显示块格式：每个公式用独立行的 `\[` 和 `\]` 包裹，多公式之间保留空行，便于复制到支持 LaTeX 的编辑器。

本地表格结合自适应局部对比度、多尺度水平/垂直线检测与文字位置推断，改善浅色、短线、断线和部分边框表格的行列还原，并保留空白内列。只有外框的表格会继续按文字行分组。倾斜表格及复杂合并单元格仍需校对。

在「设置 → 关于」点击「检查更新」会检查 GitHub 正式版本；发现更高版本后后台下载，验证大小和 SHA256，随后自动安装到当前程序目录并重新启动。更新沿用已保存的网络代理设置，失败时在关于页显示原因并可重试。安装包只接受本项目对应版本的 `SightOCR-Setup-版本号.exe`，缺少有效校验信息时不执行安装。发布新版本时应同时上传 `SHA256SUMS.txt`。普通安装遇到目标目录内正在运行的程序会提示确认，确定后自动关闭并继续安装；完成页默认勾选启动。

翻译支持 Bing、百度、腾讯、OpenAI、NVIDIA；Mistral 仅用于 OCR。默认 Bing 使用微软网页翻译的兼容接口，已替换实测返回 404 的旧 Edge 授权地址。云端服务需要网络及对应权限/配额，除 Bing 外需在设置中填写相应密钥。网页接口的可用性取决于服务方。相同源/目标语言保持原文。

新增服务在「设置 → 接口」中分别配置 API Key、Base URL 和模型。三家服务的凭据独立保存；OpenAI 与 NVIDIA 各自共用本服务的密钥和地址，并分别设置识别模型与翻译模型。

| 服务 | 用途 | 默认 Base URL | 默认模型 |
| --- | --- | --- | --- |
| Mistral OCR | OCR | `https://api.mistral.ai/v1` | `mistral-ocr-latest` |
| OpenAI | OCR、翻译 | `https://api.openai.com/v1` | 两项均为 `gpt-4.1-mini` |
| NVIDIA | OCR、翻译 | `https://integrate.api.nvidia.com/v1` | 两项均为 `qwen/qwen3.5-122b-a10b` |

地址与模型留空时回填默认值，自定义识别模型需支持图片输入。接口协议已依据官方文档核对；新增服务尚未使用真实密钥发起请求，账号权限、模型可用性及实际输出仍需在目标账号验证。

协议参考：[Mistral OCR](https://docs.mistral.ai/api/endpoint/ocr)、[OpenAI Chat Completions](https://developers.openai.com/api/reference/resources/chat)、[NVIDIA Qwen3.5](https://docs.api.nvidia.com/nim/reference/qwen-qwen3-5-122b-a10b-infer)。自定义 Base URL 应填写包含版本路径的 HTTPS 地址，例如 `https://api.example.com/v1`，无需追加 `/chat/completions` 或 `/ocr`。

## 代理

在「设置 → 代理」选择连接方式，默认跟随系统：

| 模式 | 行为 |
| --- | --- |
| 跟随系统 | 优先读取代理环境变量，随后读取 Windows 手动代理设置；不执行 PAC 脚本或 WPAD 自动发现。 |
| 不使用代理 | SightOCR 直接连接云端服务。 |
| 手动代理 | 使用指定的 HTTP、HTTPS、SOCKS5 或 SOCKS5H 代理，例如 `http://127.0.0.1:7890`。 |

代理认证的用户名和密码分别填写，密码默认遮罩；无需认证时留空。切换到系统或直连模式后，手动字段禁用并保留已填内容。该设置仅作用于 SightOCR 的云端识别与翻译，不修改 Windows 或其他应用的代理设置；保存后对新任务生效，本地 OCR 无需网络。

## 配置

- 默认保存到 `%APPDATA%\SightOCR\config.json`，避免安装目录写入权限问题。
- 首次运行导入程序/源码目录的旧 `config.json`，兼容 `paths.json` 的 `config_dir`；保留未知字段，原文件不改动。
- 兼容旧版语言显示名和 `last_source_lang` / `last_target_lang`。
- 旧配置缺少静默快捷键时自动补齐：普通识别为旧默认 F4、且翻译未占用 F5 时，将普通识别改为 F5，静默识别使用 F4。其他原有识别/翻译组合优先保留；若 F4 已被这两项占用，新静默键从 F6 至 F24 依次选择不重复的按键。已经保存的三项快捷键不会再次重置；其他程序占用导致的注册冲突仍需在设置中调整。
- 同目录临时写入、同步并原子替换；损坏配置不会被默认值覆盖，需修复后重启。
- `SIGHTOCR_CONFIG` 指定独立配置文件并跳过旧配置迁移；`SIGHTOCR_RESOURCES` 指定包含 DLL/模型的目录。
- 服务密钥和代理认证保存在本机 JSON，密码与密钥在界面掩码显示，不输出到日志；并非加密凭据库。

## 构建

安装 [Rust MSVC 工具链](https://rust-lang.org/tools/install/) 与 [Visual Studio C++ Build Tools / Windows SDK](https://learn.microsoft.com/en-us/windows/dev-environment/rust/setup)。OneOCR 资源要求 Windows x64。提交的 `Cargo.lock` 固定依赖版本。

公开源码仓库不提交 OneOCR DLL/模型、真实业务表格样本及其派生 OCR 夹具。构建或运行前，请在本地准备 `resources/oneocr/oneocr.dll`、`onnxruntime.dll` 和 `oneocr.onemodel`；这些运行时文件已包含在官方安装包中。私有样本仅用于发布前本地回归，不是应用运行依赖。

已安装 Windows 截图工具（`Microsoft.ScreenSketch`）时，可运行 `tests\ExtractOCR.bat`，经 UAC 确认后从本机应用包提取三项 OneOCR 文件到 `resources\oneocr`。脚本不联网，缺少任一文件会返回失败，并输出复制后文件的 SHA-256。请自行确认对本机组件的使用符合适用许可。

```powershell
# 从本机 Windows 截图工具准备 OneOCR 运行时（需要 UAC 确认）
.\tests\ExtractOCR.bat

cargo run --locked --bin SightOCR
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked

# 检查、测试、构建并复制资源到 dist/SightOCR
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1
# 首次可在工作区准备官方 Inno Setup 6 编译器
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/prepare-installer.ps1
# 额外生成安装包
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1 -Installer
```

若 Cargo 未加入 PATH，可使用 `$env:USERPROFILE\.cargo\bin\cargo.exe`；脚本会自动寻找该位置。安装包按用户安装。图标、版本号和 DPI manifest 嵌入 EXE；用户配置不随包发布。更新入口打开 Releases 页面，停用旧版自动下载并执行安装包的流程。

## 命令行

```powershell
# 本地识别，不上传图片
& .\target\release\SightOCR.exe --ocr .\tests\fixtures\basic.png --output .\result.txt
# 本地表格
& .\target\release\SightOCR.exe --ocr .\table.png --table --output .\table.tsv
# 按配置的服务翻译，会发送指定文本
& .\target\release\SightOCR.exe --translate 'Hello world' --from en --to zh-Hans --output .\translation.txt
```

Release 为 GUI 子系统程序，批处理建议使用 `--output` 并等待进程结束。返回码 0 为成功，非 0 为失败。

## 结构

```text
rust/main.rs       GUI / CLI 入口
rust/app.rs        应用状态、任务与系统事件
rust/ui/           工作台、设置、主题与原生窗口样式
rust/ui_smoke.rs   隔离配置的桌面界面冒烟验证
rust/worker.rs     有界队列、图像加载、引擎调度
rust/config.rs     验证、迁移、原子保存
rust/services.rs   云端 OCR、翻译、签名、令牌
rust/services/ai.rs Mistral OCR 与 OpenAI/NVIDIA 识别、翻译协议
rust/ocr.rs        OneOCR FFI 与 COM 生命周期
rust/ocr/table.rs  本地表格恢复
rust/platform.rs  截图、热键、托盘、自启、单实例
scripts/           启动与构建
resources/oneocr/  原有 DLL 与模型
```

`SightOCR.py`、`src/*.py`、`requirements.txt` 和旧打包说明保留作迁移参考，不参与 Rust 运行。`scripts/run.ps1 -Legacy` 可运行旧版，需另装旧 Python 依赖。

重构问题清单和人工回归项目见 [重构记录](docs/RUST_REFACTOR.md)，实测结果见 [验证记录](docs/VALIDATION.md)。本地复杂合并/倾斜表格仍属于启发式恢复；多屏混合 DPI、远程桌面和云端账号权限需在目标环境验证。

此前窗口缩放和截图时序修复继续保留。本次按 2.0.0 重新打包，24 个界面场景及表格、更新回归使用合成图片和响应验证，不采集桌面或调用云端 OCR。构建、安装包检查及 EXE 校验和以[验证记录](docs/VALIDATION.md)和 `dist/installer/VALIDATION.json` 为准，不能沿用同版本旧包结果。

SightOCR 第一方代码作者及版权归属为 FueTsui，采用 [MIT 许可](LICENSE)。现有 OneOCR DLL/模型及其他第三方组件保留各自的作者、版权和许可。

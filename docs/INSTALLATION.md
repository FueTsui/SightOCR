# Windows 安装包

当前发布文件为 `dist/installer/SightOCR-Setup-2.0.0.exe`，按要求沿用 2.0.0 版本号，包含程序、OneOCR 引擎和模型、README、LICENSE 与文档。程序和安装包的产品名称、文件描述统一为 SightOCR，第一方发布者及版权作者为 FueTsui。安装到当前用户的 `%LOCALAPPDATA%\Programs\SightOCR`，无需管理员权限；要求 Windows 10/11，支持运行 x64 程序的系统。Arm64 模拟环境尚未验证。相同版本号的不同构建请通过发布文件校验和区分。

本次构建补齐多语言字体覆盖，并用 Windows 原生编辑控件处理复杂文字连写及双向排版；修复识别完成后正文黑屏、旧画面残留及语言菜单遮挡问题，并取消不变正文的重复擦除和重绘，避免日期、时间、数字和其他识别文字持续闪烁。修复正文未聚焦时无法滚轮滚动的问题，原文和译文按鼠标所在位置各自滚动，语言菜单不会穿透滚动正文。取消识别/翻译会终止独立任务进程并立即恢复界面。百度、腾讯公式的 LaTeX 显示块格式和设置「代理」名称继续保留。版本继续使用 2.0.0；同版本重新打包需要手动运行新版安装包覆盖更新。默认客户区为 740×680 逻辑像素，最小为 740×580，支持调整大小和最大化。

安装时可选择开始菜单快捷方式，桌面快捷方式默认不勾选。完成页默认勾选运行 SightOCR。手动安装或卸载发现目标安装目录的 SightOCR 正在运行时，会询问是否自动关闭；点击「确定」后先请求程序正常退出，旧版或无响应程序在等待 5 秒后自动结束，再继续安装。点击「取消」会退出安装并保留正在运行的程序。关闭程序会丢失未保存的识别内容。

普通静默安装不自动启动程序，未获关闭确认时也不结束运行中的程序。程序内更新使用 `/UPDATE /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /DIR="当前程序目录"`，等待程序正常退出最多 120 秒，为进行中的网络任务清理预留时间，随后覆盖安装并自动启动一次新版；如果程序未能退出，安装器会停止，不强制结束或替换正在使用的文件。所有关闭操作均核对进程的完整 EXE 路径，不关闭其他目录中的 SightOCR。

后台下载使用已保存的代理配置，仅接受官方发布中版本匹配的安装包，并验证文件大小、SHA256 和 Windows 程序头。下载失败会清理临时文件；交接成功后由临时目录内的独立更新程序等待原进程退出、再次校验安装包并跟踪安装退出码。安装失败时显示原因、提供安装日志路径并尝试重新启动 SightOCR。交接后的文件保留在用户临时目录，供错误排查。发布时应上传生成的 `SHA256SUMS.txt`；仅更高的正式版本会触发自动更新，同版本重打包不会触发升级。

可在 Windows「已安装的应用」中卸载。卸载移除本次安装的文件和快捷方式，保留 `%APPDATA%\SightOCR\config.json` 及用户自行创建的文件；仅当开机启动项精确指向本次安装路径时移除该启动项。配置包含本机服务凭据，请按需自行保留或删除。

## 构建

```powershell
# 仅在工作区准备官方便携编译器，不修改 PATH、文件关联或安装登记
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/prepare-installer.ps1
# 检查、测试、构建便携目录并生成正式安装包
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build.ps1 -Installer
```

也可通过 `-IsccPath C:\Tools\Inno\ISCC.exe` 或进程环境变量 `SIGHTOCR_ISCC` 指定编译器。脚本还会查找 PATH、工作区 `target/tools/innosetup-6.7.3/ISCC.exe` 与常见 Inno Setup 6 安装目录。

本次使用 [Inno Setup 官方 6.7.3](https://github.com/jrsoftware/issrc/releases/tag/is-6_7_3)。准备脚本校验官方发布文件的 SHA-256 `9C73C3BAE7ED48D44112A0F48E66742C00090BDB5BEF71D9D3C056C66E97B732`，并验证 Pyrsys B.V. 的 Authenticode 签名；记录保存在 `target/tools/inno-provenance.json`。此校验针对编译器下载包，SightOCR 安装包目前未配置代码签名。

编译器沿用 [Inno Setup 许可](https://github.com/jrsoftware/issrc/blob/is-6_7_3/license.txt)，官网另有[商业支持许可说明](https://jrsoftware.org/isorder.php)。中文消息文件来自同一官方版本仓库的 `Files/Languages/Unofficial/ChineseSimplified.isl`，保留原作者注释，SHA-256 为 `7D544B9BB1D142CFA11F2E5D3CC8ABE2E55F8E066C5124E3772675AA236E1278`。

## 隔离验证

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-installer.ps1
# 仅检查安装器关闭/取消/更新重启协议，使用小型合成程序
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-installer.ps1 -ShutdownOnly
# 额外验证等待 120 秒超时后不会强制结束程序或替换文件
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-installer.ps1 -ShutdownOnly -VerifyUpdateTimeout
```

验证脚本将当前便携目录编译为测试专用安装包，目标为随机的 `target/installer-smoke-*` 子目录。测试关闭快捷方式和安装登记，保留正式安装包的文件负载、复制/覆盖、静默启动限制、进程检查和卸载逻辑。它执行两次静默安装、逐文件 SHA-256 比对、使用包内 EXE/模型识别仓库 TSV 夹具，再执行卸载并检查合成配置保留。

脚本先用 `scripts/installer/ShutdownFixture.cs` 构建独立合成程序，验证确认后正常退出、取消后保留程序并退出安装、旧版兼容关闭、普通静默安装不擅自关闭、更新后仅启动一次，以及其他目录的同名程序保持运行。仅向本测试启动的安装进程发送确认按钮消息。测试不安装到真实 AppData，不修改用户程序配置或剪贴板；进程操作只针对隔离目录内的合成程序。

报告保留在对应测试目录的 `report.json` 和 `shutdown/report.json`。该测试不等同于正式安装包在真实开始菜单、安装登记与实际用户环境中的完整验收；正式发布包应在最终构建完成后重新编译。

2.0.0 历史预检（2026-09-06）已完成：`target/installer-smoke-b41489f4481747ccafc68968fc7b0d3d/report.json` 各项通过，包内模型输出 `Name\tAmount`、`Alpha\t100`、`Beta\t200`。当时正式配置的安装脚本也已编译成功；当前 2.0.0 重打包以完整构建后重新生成的文件和 [验证记录](VALIDATION.md) 为准，不能沿用此前同版本文件的校验值。

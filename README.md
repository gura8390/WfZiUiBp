# 文字识别 WfZiUiBp

Windows x64 离线文字识别工具。0.2.0 按 Umi-OCR Paddle v2.1.5 的首屏、双栏工作区和标签导航重做界面，沿用原版图标与五套主题。桌面、截图、任务调度和接口用 Rust 实现，CPU 推理由随包的 ONNX Runtime 运行。

## 使用

从 [GitHub Release v0.2.0](https://github.com/gura8390/WfZiUiBp/releases/tag/v0.2.0) 选择 Windows x64 安装程序或便携 ZIP。安装程序为当前用户创建开始菜单入口并提供卸载。便携包解压后运行 `WfZiUiBp.exe`，程序、`models` 和同目录运行库需一起保留。

按 **F4** 后拖选屏幕区域，松开鼠标识别。`Esc` 和右键取消选区。关闭主窗口默认留在托盘，Windows 将图标折叠时，从时钟旁的上箭头展开。托盘菜单提供截图、粘贴、扫码、打开主窗和退出。`Alt+F4` 保持系统行为。

- **截图OCR**的左侧是图片与文本框，右侧切换设置和记录。支持重截上一选区、粘贴、复制文本框及编辑识别文字。
- **批量OCR**和**批量文档**先添加待处理文件，再按开始。可预览、设置忽略区域、暂停和停止，以及按选定格式导出。
- **文档**支持 PDF、XPS、CBZ、EPUB、FB2 和传统无 DRM 的 MOBI，支持 PDF 文字层优先、密码 PDF、双层和纯文本 PDF 导出。EPUB、FB2 和 MOBI 以重排版式呈现，插图另页交给 OCR。忽略区域按原图像素设置。纯文本 PDF 从系统中选择可嵌入的字体。
- **二维码**支持截图、剪贴板和图片扫描，以及生成二维码并保存。扫描底层使用 Rust 的 ZXing 移植实现。
- **全局设置**保存在 `%LOCALAPPDATA%\WfZiUiBp\settings.json`。主题提供赛博朋克、珍珠白、云墨黑及两套 Solarized。标签栏可置顶窗口、锁定标签。`Ctrl+T` 新建标签，`Ctrl+W` 关闭当前标签。登录启动可单独开启，默认关闭。
- **无障碍**通过 Windows UI Automation 提供按钮、队列、进度、设置状态和文本输入。截图记录和二维码文本可通过语义动作读写。忽略区域也可通过数字坐标输入。

新建桌面配置默认使用 PP-OCRv6 Tiny，基于本机中英样本的耗时、内存和错字率选择。传统六套模型在设置的语言下拉框保留，日语、韩语和西里尔文可选各自的模型。命令行和 API 默认沿用简体中文旧模型，便于替换原调用。模型文件及校验值见 [上游来源与许可](NOTICE.md)。

## 命令行和 API

```powershell
# 让正在运行的程序截屏
.\WfZiUiBp.exe --screenshot

# 在后台识别一张图片并输出文件
Start-Process -FilePath .\WfZiUiBp.exe -ArgumentList '--path "C:\test\input.png" --language v6_tiny --output "C:\test\result.txt"' -Wait -WindowStyle Hidden

# 本机 HTTP 服务，默认监听 127.0.0.1:1225
.\WfZiUiBp.exe --serve --port 1225 --threads 4
```

`--show`、`--hide`、`--quit`、`--reload`、`--repeat`、`--clipboard` 可控制同一桌面实例。用 `--hide` 启动时，先初始化原生托盘和 F4，等首次显示或有识别结果才创建主窗口。`--path`、带 `screen=0 rect=x,y,w,h` 的 `--screenshot`、`--qrcode_read` 和 `--qrcode_create` 可单独调用。GUI 子系统程序可用 `Start-Process -Wait` 等待批处理结束。`--output`、`--output_append`、`--clip` 和 `--json` 可控制输出。

HTTP 服务提供 Umi 风格的 `/api/ocr`、`/api/ocr/get_options`、`/api/qrcode`、`/api/doc/*`。文档接口支持 PDF 和 CBZ。上传、结果读取和下载需使用同一服务进程。监听默认限于本机。

## 构建和验证

```powershell
.\tools\build.ps1 -Package
```

在源码目录运行构建和测试命令。构建使用 Rust 1.95 及以上、MSVC x64 和 Windows SDK。脚本先执行锁定依赖的 release 测试和编译，再打包程序、模型与运行库。发布构建可用 `-RemapBuildPaths` 替换编译器记录的本机路径。便携目录与 ZIP 各有 SHA-256 清单。安装包的构建步骤见 [安装程序源码说明](installer/README.md)。

- [0.2.0 性能对照](docs/性能对照.md)
- [2026-11-06 本机复测报告与原始数据](verification/retest-20261106/复测报告.md)
- [0.2.0 功能与验证范围](docs/验证范围.md)

兼容范围以本说明列出的工作流和接口为准。每种功能页在本版保留一个标签。原版的独立重复标签、Python/QML 插件、完整配置迁移和文档逐页范围属于仍有差异的部分。

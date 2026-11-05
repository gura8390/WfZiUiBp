# Windows 安装包

安装程序面向 x64 Windows 10 和 Windows 11。应用文件安装到当前用户的 `%LOCALAPPDATA%\Programs\WfZiUiBp`，从开始菜单打开，也可在安装时选择创建桌面快捷方式。安装完成后由用户手动启动应用。卸载可从 Windows「已安装的应用」或开始菜单发起，保留 `%LOCALAPPDATA%\WfZiUiBp\settings.json`。

安装和卸载前请先从托盘退出运行中的文字识别。安装器在提交前暂存完整应用文件，更新时把原目录中的额外文件保留在同级备份目录，并在安装详情中显示其路径。安装程序采用 NSIS x86 Unicode 启动器，应用和推理运行库为 x64。

## 从便携产物构建

准备与发布版本一致的便携目录及其中的 `SHA256SUMS.txt`，以及 7-Zip 和官方 NSIS 3.12 ZIP。编译器 ZIP 的 SHA-256 为 `56581f90db321581c5381193d796fffcf2d24b2f8fed2160a6c6a3baa67f2c4f`。[NSIS 3.12 官方下载](https://sourceforge.net/projects/nsis/files/NSIS%203/3.12/nsis-3.12.zip/download)

```powershell
.\tools\build-installer.ps1 -PayloadRoot .\dist\WfZiUiBp -CompilerZip C:\Downloads\nsis-3.12.zip
```

构建脚本校验 NSIS ZIP，再把编译器解压到项目内的临时目录。它编译安装包，用 7-Zip 静态提取并逐项核对应用载荷的 SHA-256，随后输出 `dist\WfZiUiBp-<版本>-setup-x64.exe` 和对应的 `.sha256`。这些检查不启动安装程序。NSIS 压缩使用 zlib。[NSIS 许可](https://nsis.sourceforge.io/Docs/AppendixI.html)

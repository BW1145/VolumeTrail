# 从源码构建

这些步骤用于开发和自行打包。运行已构建的便携程序只需要 Windows，
不需要安装 Rust、LLVM-MinGW、Node.js 或 Python。

## 环境

- Windows x64；扫描目标为 NTFS 磁盘。
- PowerShell 和 Git。也可以下载源码 ZIP，解压后进入项目根目录。
- 已验证工具链：Rust 1.98.1，`x86_64-pc-windows-gnu`，
  LLVM-MinGW 20260908 msvcrt x86_64。
- 安装工具链和首次下载 Cargo 依赖需要网络。

开发工具保存在项目 `.tools/`，编译产物在 `target/`，便携包在 `dist/`。
可以把整个项目放在非系统盘；这些目录均由 `.gitignore` 排除。
构建和测试使用普通权限，实际扫描需要管理员权限。

## 1. 获取源码

在准备保存项目的目录运行：

```powershell
git clone https://github.com/BW1145/VolumeTrail.git
Set-Location VolumeTrail
```

以下命令都在包含 `Cargo.toml` 和 `build.ps1` 的项目根目录执行。
仓库访问权限由 GitHub 仓库可见性和登录账号决定。

## 2. 安装项目内的 Rust

安装器来自 [Rust 官方安装页](https://rust-lang.org/tools/install/)。
使用 GNU 工具链配合下面的 LLVM-MinGW，不需要另装 Visual Studio C++ Build Tools。

```powershell
$OutputEncoding = [Console]::OutputEncoding = [Text.UTF8Encoding]::new()
$root = (Get-Location).Path
$env:CARGO_HOME = Join-Path $root '.tools\cargo'
$env:RUSTUP_HOME = Join-Path $root '.tools\rustup'
$env:TEMP = Join-Path $root '.tools\tmp'
$env:TMP = $env:TEMP
New-Item -ItemType Directory -Path $env:TEMP -Force | Out-Null
Invoke-WebRequest 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe' -OutFile '.tools\rustup-init.exe'
& '.\.tools\rustup-init.exe' -y --no-modify-path --profile minimal --default-host x86_64-pc-windows-gnu --default-toolchain 1.98.1
```

安装器本身使用 MSVC 版本，安装的编译工具链由 `--default-host` 指定为 GNU。
`--no-modify-path` 保持系统 PATH 设置不变；后续由构建脚本设置当前进程的环境。
确认安装成功后继续。所有工具应位于本项目的 `.tools/` 内。

## 3. 安装 LLVM-MinGW

来源：[LLVM-MinGW 20260908 官方发布](https://github.com/mstorsjo/llvm-mingw/releases/tag/20260908)。
选择 `llvm-mingw-20260908-msvcrt-x86_64.zip`，不要选择 ARM64 或 UCRT 包。

```powershell
$url = 'https://github.com/mstorsjo/llvm-mingw/releases/download/20260908/llvm-mingw-20260908-msvcrt-x86_64.zip'
$archive = '.tools\llvm-mingw-20260908-msvcrt-x86_64.zip'
Invoke-WebRequest $url -OutFile $archive
$expected = '341cc9786b54956467ac19a02fdb00749307eac133da56108f643e4189db810b'
if ((Get-FileHash $archive -Algorithm SHA256).Hash -ne $expected) {
    throw 'LLVM-MinGW archive SHA-256 mismatch'
}
Expand-Archive -LiteralPath $archive -DestinationPath '.tools'
Test-Path '.tools\llvm-mingw-20260908-msvcrt-x86_64\bin\x86_64-w64-mingw32-clang.exe'
```

最后一条应输出 `True`。解压后目录名称要与 `build.ps1` 中使用的目录一致。

## 4. 检查、测试和打包

```powershell
.\build.ps1 check -CargoArgs '--locked'
.\build.ps1 test -CargoArgs '--locked','--lib','--test','store'
.\build.ps1 package
```

保留仓库中的 `Cargo.lock`，让依赖版本与已验证版本一致。
测试使用临时数据，不需要扫描真实磁盘。
上述命令默认编译 `debug` 版本；`package` 会去除调试信息，生成：

- `dist/VolumeTrail/VolumeTrail.exe`：图形界面，无常驻控制台窗口。
- `dist/VolumeTrail/VolumeTrail-cli.exe`：命令行程序，保留终端输出。

如需优化构建：

```powershell
.\build.ps1 release
```

该命令生成 `target/release/volumetrail.exe`，不更新 `dist/` 便携包。
`package` 当前只打包 debug 构建，不要把两者混用。

## 5. 运行与分发

```powershell
.\dist\VolumeTrail\VolumeTrail.exe
```

程序旁的 `data/` 在运行时创建，用来保存配置、文件路径、磁盘索引和历史。
发布便携包时，只打包两个 `.exe`；不要打包自己使用过的整个目录。
首次扫描为全量索引，后续扫描按 USN 日志增量更新。
首次扫描耗时取决于磁盘规模；查看已有记录不需要管理员权限。

## 构建问题

- 提示工具不存在：核对 `.tools/cargo/bin/cargo.exe` 和 LLVM-MinGW 的目录结构。
- PowerShell 阻止执行脚本：可以在当前窗口运行
  `Set-ExecutionPolicy -Scope Process Bypass`，再执行上述构建命令；不修改机器级策略。
- 安全软件拦截编译产物：记录被拦截的文件路径和检测名称，核对来源后处理；
  构建步骤不要求关闭防护。已有诊断记录见 [toolchain.md](toolchain.md)。

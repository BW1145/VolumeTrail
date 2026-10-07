# 盘迹 · VolumeTrail

轻量的 Windows NTFS 磁盘空间变化记录工具，用于查明哪些目录占用了更多空间，并为手动清理和 AI 分析提供记录。

## 功能

- 登录后自动扫描配置的磁盘，保存结果后退出。
- 首次建立全量索引，后续通过 USN 变化日志进行增量扫描；同一文件的重复事件会先合并。
- 系统繁忙时暂停，使用后台资源优先级；支持手动扫描和停止。
- 查看空间趋势、两个时间点之间的目录净变化、文件明细和当前占用排行。
- 目录下钻显示总变化、本层变化及子目录构成；已移除的目录仍可查询历史。
- 打开文件位置，使用命令行读取文本或 JSON 结果。
- 保存扫描耗时、CPU 用时、平均 CPU 占用和进程峰值内存。
- 按容量整理历史明细，保留趋势时间点；索引、历史和备份的占用分别显示。

程序用于记录和定位空间变化，用户文件的清理由用户决定并执行。

## 运行

便携包包含 `VolumeTrail.exe` 和 `VolumeTrail-cli.exe`。双击前者打开界面。
程序和 `data/` 放在一起，可以安装到非 C 盘。便携包运行时只依赖 Windows 系统组件。

原始 NTFS 扫描需要管理员权限。界面会在启动扫描时请求权限，查看已有记录和命令行查询使用普通权限即可。
自动扫描使用 Windows 任务计划程序，由设置页开启。多个磁盘按顺序扫描。

```powershell
.\VolumeTrail-cli.exe query summary --drive C --limit 20 --json
.\VolumeTrail-cli.exe query growth --drive C --from 8 --to 16 --json
.\VolumeTrail-cli.exe query folder --drive C --path 'C:\Users\Example\AppData\Local' --from 8 --to 16 --json
```

比较命令中的扫描编号应替换为已有记录的编号。完整查询说明见 [docs/query.md](docs/query.md)。

## 开发

使用 Rust、egui/eframe、SQLite 和 Windows API。当前版本为 0.2.2。

从源码构建需要 Windows x64、Rust GNU 工具链和 LLVM-MinGW。
完整的下载、校验、安装、测试和打包步骤见 [构建说明](docs/build.md)。
普通用户运行便携包不需要安装这些开发工具。

`build.ps1` 使用项目 `.tools/` 内的工具链。完成构建说明中的安装后运行：

```powershell
.\build.ps1 test -CargoArgs '--lib','--test','store'
.\build.ps1 package
```

`package` 生成 `dist/VolumeTrail/` 中的 GUI 与 CLI 便携程序。
当前便携包采用去除调试信息的 debug 构建；优化构建使用 `release` 命令。
发布便携包时只包含程序文件，不包含运行后生成的 `data/`。

## 数据与文档

SQLite 索引、扫描历史和配置位于便携程序旁的 `data/`。
历史容量上限只限制文件明细与目录汇总；当前索引、趋势与性能记录、迁移备份分别计量。

- [当前设计与实现](docs/plan.md)
- [0.2.1 验证记录](docs/progress-2026-10-03.md)
- [查询接口](docs/query.md)
- [产品设计](docs/design.md)

仓库保存源码、测试和文档。扫描数据库、个人配置、构建缓存、便携产物及本机源码备份由 `.gitignore` 排除。

本机截图、诊断导出和个人笔记统一放在 `local-notes/` 或 `evidence/`。
提交文档和测试时使用 `Example` 等通用用户名以及相对项目路径；环境变量、凭据与私钥在忽略规则内。

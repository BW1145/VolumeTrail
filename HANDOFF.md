# VolumeTrail 项目交接

当前实现与验证见 `docs/plan.md` 和 `docs/progress-2026-10-03.md`。本文保留 2026-09-13 的交接快照。

更新日期：2026-09-13。工作区：项目根目录。

用户当前要求：保存进度与发现，由其他 AI 接手。当前开发在本交接点停止。

## 当前结论

项目已有可编译的数据模型、SQLite 数据层和 5 项通过的测试。
尚未成为可使用的桌面软件：扫描器、界面、程序入口、自启及发行包均待实现。
`target/debug/deps` 下的 exe 是测试程序，不能当作产品交付。

先读本文，再读 `docs/design.md`、`src/model.rs`、`src/store.rs` 和 `tests/store.rs`。
原计划在 `docs/plan.md`，最新完成度以本文和实际源码为准。

## 用户期望与已确定要求

软件用于解释磁盘空间为何增长，重点是 C 盘，也应支持选择其他 NTFS 磁盘。
用户优先关心资源占用、游戏性能、清晰易用的中文操作界面。

- 开机后的首次登录自动扫描一次，保存结果后退出；用户手动打开时查看记录，也能手动扫描和停止扫描。
- 常规运行采用一次性任务。持续后台服务、白天定时扫描、关机扫描不属于第一版。
- 首次以 MFT 元数据建立基准，此后优先读取连续的 USN 变更日志并核对变化；日志失效或断档时重新建立基准。
- 一两分钟是理想扫描时间，最好少于五分钟。五分钟是性能目标，不能实现成强制超时或开机五分钟窗口。
- 系统负载高时让步，使用低优先级、分批处理。用户明确不需要维护游戏进程清单。
- 程序、配置、索引、历史和自身日志都跟软件放在一起，运行数据放 `data/`。用户会把软件放在非 C 盘。
- 记录文件元数据与变化，不备份文件内容。历史按总容量预算淘汰最旧记录，保留当前索引及必要的最新状态。
- `docs/design.md` 中的 512 MiB 是实现时提出的初始预算，尚无实测依据，不能写成用户明确指定的数值。
- 展示增长与缩减排行、历史、文件夹浏览，以及打开对应目录或在资源管理器中选中文件的操作。
- 第一版职责是记录与定位空间变化；用户文件清理功能暂不实现。
- 区分文件逻辑长度与实际分配空间；硬链接按文件身份去重，保留路径关联；不跟随目录联接递归到其他位置。
- 显示文件统计与卷已用空间之间未解释的差额，避免伪造精确归因。
- 取消或读取失败不能损坏上一次完整结果，不能把无法读取误判成删除；检查点与数据一起提交。
- 自动扫描成功时安静退出，失败信息在下次打开软件时可查看。
- 成品目标是便携式 Windows 程序，运行时无需安装 Rust、Python、Node、.NET 或独立数据库。

## 架构与现有代码

采用 Rust、egui/eframe 桌面界面和内嵌 SQLite。拟由同一个 exe 提供界面模式和扫描模式；
自动扫描不初始化图形界面。Windows 任务计划程序负责登录触发，原始卷读取需要相应管理员权限。
这些运行模式和任务计划目前只是设计，尚未编码。

| 文件 | 当前内容 |
| --- | --- |
| `Cargo.toml` / `Cargo.lock` | Rust 工程及已解析的依赖；eframe 0.33.3、rusqlite 0.37.0、ntfs 0.4.0、windows 0.62.2 等 |
| `.cargo/config.toml` | 编译并行数 2；Windows GNU 静态 CRT 与自包含链接参数 |
| `build.ps1` | 项目内工具链、缓存、TEMP 和 target 路径；使用 LLVM-MinGW，构建进程设为 BelowNormal |
| `src/lib.rs` | 仅导出 model、store |
| `src/model.rs` | Entry、Link、Checkpoint、Mutation、ScanMode、ScanOutcome、ScanProgress |
| `src/store.rs` | SQLite 表结构、扫描暂存与事务提交、索引、变化记录、文件夹汇总、历史查询及裁剪代码 |
| `tests/store.rs` | 5 项数据层测试 |
| `docs/design.md` | 产品与正确性要求 |
| `docs/toolchain.md` | 工具来源、校验值与 360 告警记录 |
| `docs/verification-2026-09-13.md` | 本次交接核验结果 |

数据层公开接口包括 `Store::open`、`begin_stage`、`stage`、`discard_stage`、
`commit_stage`、`publish`、`checkpoint`、`pending`、`totals`、`changes`、
`reports`、`children`、`prune` 和 `used_bytes`。

暂存期间持有 `BEGIN IMMEDIATE` 事务，成功提交同时更新索引和 USN 检查点，取消调用回滚。
SQLite 使用 WAL；节点以卷身份和文件 ID 为键。文件信息还以 JSON 存在 payload 中。
第一次基准不生成海量“新增”变化记录；后续结果才与已有基准对比。

## 已验证的结果与范围

2026-09-13 在项目目录运行 `./build.ps1 test`，退出码 0，5 项测试通过：

1. 相同文件身份重复出现只计一次，数据库重新打开后数据与检查点保留。
2. 文件从 100 增至 400 字节报告 +300，删除 200 字节文件报告 -200，重命名的增长为 0。
3. journal ID 变化时拒绝部分增量，保留之前的大小与检查点。
4. 取消暂存后保留已发布索引与检查点。
5. 目录移动只生成该目录的移动记录，不把未变化的子文件当作增长。

这些测试使用临时 SQLite 数据库和人工构造的元数据。
尚未验证真实 NTFS 硬链接、原始磁盘扫描、真实取消、目录权限、稀疏与压缩文件、
文件持续写入、USN 日志断档、发行依赖、界面或资源占用。
“文件身份去重测试通过”不能扩展成“真实硬链接扫描已实现”。

构建提示依赖 `binrw 0.11.3` 有 Rust 未来不兼容警告；当前构建和测试成功。
可用 `cargo report future-incompatibilities --id 1` 查看具体报告，需先配置同样的项目工具链环境。

## 工具链与 360 事件

工具全在项目的 `.tools/`，Rust 工具链目录为
`rustup/toolchains/stable-x86_64-pc-windows-gnu`。执行 `build.ps1` 会自动设置环境。

2026-09-08，360 隔离了 `.tools/w64devkit` 下的两个文件，截图检测名为
`Win64/Heur.Generic...`，合计 2.34 MB；截图未显示完整文件名。
当时编译器启动返回拒绝访问。后续文件检查未找到 gcc.exe 及其交叉编译器入口，
但这不足以将截图中的两个条目精确映射到具体文件。

2026-09-12，原 w64devkit 下载包的 SHA-256 与作者 GitHub 发布资产一致：
`9208c19755cd4964b7915b9afcf02c66d493a4c870c4b3e83f6c538d9c1237a5`。
这说明下载包与发布资产一致，不构成安全性结论。隔离文件保持原状。

当前构建采用 LLVM-MinGW 20260908 msvcrt x86_64：

- 来源：https://github.com/mstorsjo/llvm-mingw/releases/tag/20260908
- 文件：`llvm-mingw-20260908-msvcrt-x86_64.zip`
- SHA-256：`341cc9786b54956467ac19a02fdb00749307eac133da56108f643e4189db810b`
- 下载后先核对 GitHub 发布资产 digest，再解压；2026-09-13 完整测试命令成功。
- 活跃编译器路径：`.tools/llvm-mingw-20260908-msvcrt-x86_64/bin/x86_64-w64-mingw32-clang.exe`。

原 w64devkit 目录与下载包仍保留作事件记录，构建脚本已经使用 LLVM。
不要恢复隔离文件、关闭防护或加白名单。当前没有证据可以宣布告警一定是误报。

## 扫描实现的研究发现

扫描代码尚未落地。`ntfs 0.4.0` 已由 Cargo 下载，但尚未被现有代码调用。

- `.tools/ntfs-reader` 是先前检查的上游源码。其 `Mft::new` 会把整个原始 MFT 载入 Vec，
  可能占用大量内存，不适合直接作为低资源基线实现。
- `ntfs 0.4.0` 接受 Read + Seek，可读取原始卷或镜像，提供属性与扩展属性解析。
  源码位于 `.tools/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ntfs-0.4.0`。
- 它的 `examples/ntfs-shell/main.rs` 和 `sector_reader.rs` 展示了 Windows 原始卷和对齐读取。
  需要验证分块缓存、MFT 枚举、属性流分配空间及持续写入时的元数据一致性，不能预先承诺速度。
- USN 读取、卷身份及 journal 连续性校验需要 Windows API 集成；已有模型保存检查点和 pending IDs，
  但目前没有算法实际生产这些值。
- 先前普通用户环境下 `fsutil usn queryjournal C:` 成功，`fsutil fsinfo ntfsinfo C:` 拒绝访问。
  当前 Windows 提权能力仍需接手者针对实际操作确认；工具沙箱提权不等于 Windows 管理员令牌。
- 参考入口：`https://github.com/chuunibian/delta`、`https://github.com/windirstat/windirstat`、
  `https://github.com/xangelix/edirstat`。引入代码前核实上游版本与许可证，当前未复制其产品实现。

曾向子代理提出下列扫描函数接口，但该代理已停止，未产出 scanner.rs；这是草案接口：

```rust
pub fn scan(
    root: &str,
    previous: Option<&Checkpoint>,
    pending: &[u64],
    emit: &mut dyn FnMut(Mutation) -> anyhow::Result<()>,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(ScanProgress),
) -> anyhow::Result<ScanOutcome>;
```

建议让扫描器流式调用 `stage`，成功后 `commit_stage`，失败或取消时 `discard_stage`。
先前完整扫描必须足够完整才能提交：在当前 store 语义中，完整快照缺失的旧节点会被当作删除。

## 接手时优先处理的缺口

1. 实现并验证 MFT 基线及 USN 增量。应处理日志跨扫描期间变化、FRN 序号复用、打开写入的文件、
   目录重命名、扩展属性、ADS、硬链接和卷盘符变化；用真实 NTFS 临时夹具核对结果。
2. 检查数据层的未覆盖范围。`used_bytes()` 当前是数据库已使用页面估算，
   不包含 WAL、SHM、日志或整个 data 目录，尚不能兑现“总数据容量上限”。
   `prune`、文件夹查询、报表与磁盘空间回收尚无专项测试。
3. `changes`、`reports`、`children` 分别有 2000、500、5000 条查询限制。
   界面设计时需要分页或明确显示范围，避免看起来像完整列表。
4. 当前每次发布都加载目录映射并重新汇总文件夹。全量基线还同时暂存数据与保存现有索引，
   并把元数据重复存为列与 JSON。内存、写盘量、数据库大小和增量耗时均需要实测与必要优化。
5. `Entry.links` 目前仅存储；还没有真实扫描端的稳定主路径选择及所有链接路径的浏览语义。
   目录节点占用未计入 `totals` 的文件合计；需要明确元数据占用与未解释差额口径。
6. 增加程序入口、中文界面、数据目录初始化、状态/错误记录、单实例扫描控制和手动停止。
   `Entry.modified` 的时间单位尚未形成明确约定，应在扫描和 UI 对接前确定。
7. 实现低优先级和系统高负载让步、登录后一次性任务、权限流程，验证进程退出。
8. 构建发行包并在真实 C 盘测试扫描耗时、峰值内存、读写量、数据文件大小和增量速度；
   检查运行依赖，测试界面与打开目录操作，满足第一版可使用的交付目标。

以上缺口是继续工作的线索，不是完成过的审查结论。保持已有测试通过，按实际失败与需求补测试。

## 如何继续

```powershell
Set-Location '<project-root>'
.\build.ps1 test
# 实现程序入口后才具备应用发行条件
.\build.ps1 release
```

Git 当前分支为 `develop`，尚无提交；项目源码和文档目前均未跟踪。
`.tools/` 和 `target/` 在忽略规则内。现有源码已经保存到磁盘，另附源码压缩快照；
快照包含源码、锁文件、构建脚本和文档，开发工具及构建缓存留在原工作区。

本交接核验时未查到 cargo、rustc、clang、ld.lld、diskhistory 进程。
原编译会话句柄已失效，因此本次重新运行测试取得了上述最终结果。
接手者应以工作区文件和新执行结果为准，避免把旧会话中的计划或进度描述当成成品证据。

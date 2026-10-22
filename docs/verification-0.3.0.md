# 0.3.0 候选实现与验证交接

当前状态：**[v0.3.0 已正式发布](https://github.com/BW1145/VolumeTrail/releases/tag/v0.3.0)，[PR #1](https://github.com/BW1145/VolumeTrail/pull/1) 已合并 main**。最终源码对应的 [Windows CI #39587285000](https://github.com/BW1145/VolumeTrail/actions/runs/39587285000) 已通过全目标测试与隔离 NTFS 验证；包与源码来源关系见 Release 附件。

以下为发布前候选验证快照，保留当时证据与未覆盖项；“未发布”和阻塞描述不代表当前发布状态。真实桌面交互、真实登录触发等不因正式发布而补算通过，范围见 [ci-ntfs-validation.md](ci-ntfs-validation.md)。使用说明见 [README](../README.md)。

## 发布前检查点

状态：代码和 Windows 审查包已准备；检查点时完整验收尚未完成。
分支 `codex/overview-cleanup`；远端基线 `ae8a1b355824dd82ed3fec668e389e7d1f32c9c0` 已合入。
未合并 main、未发布 Release、未替换已安装程序、未修改现有登录任务。

## 实现

- GUI/CLI 共用 overview 服务，操作前/核验点分组，当前索引容量独立于已保留历史点；历史列表按操作分组，观测菜单保留逐点精确时间和选择。
- 规则保存在独立 SQLite 表，默认暂停；修订检查、预览、启用、执行和结果共用业务模块。
- Windows 独立模块逐级固定目录句柄，检查重解析属性和完整文件身份，独占叶文件句柄后设置删除 disposition。
- 登录流程沿用原全局锁，成功扫描后才清理；每日声明防重复；手动扫描不接删除。
- 核验在 staging 前强制 RequireIncremental，关联元数据和扫描提交同事务，取消标志不重置。
- JSON 契约、分页、错误/退出状态、预览绑定和 request_id 幂等；中断后不会重放。
- pending 意图先持久化；终态明细最多 200 条/次（当前未决意图额外保护）、保留 30 天；汇总最多 1000 次/365 天。
- 候选最多 10000 项/约 8 MiB JSON；遍历最多 20 万项、128 层，达到限额明确截断；截断不能自动启用。

目录句柄最初只请求 READ_ATTRIBUTES，新增真实 Windows 重命名回归发现不足。现已同时请求
LIST_DIRECTORY，固定期间重命名失败的回归通过。这是阶段三之后的实质安全修复。

## 已验证

默认构建下 `cargo test --locked --offline`：68 项通过；另 1 项原始 NTFS 测试在本地 ignored。日志在本地 `evidence/tests-final.txt`。
当前新增：删除上次核验点不回退、旧 run 取消不影响新任务、201 条未决汇总、1000 次/365 天边界、执行中 UI 回执保护、中文状态与原生目录选择入口。
默认比较保存实际上一成功点；旧库只回填全局 ID 连续的可证明区间，缺失或未核验清理对应区间保持未知。

其中现有两项 production_* 测试在未指定外部生产库时直接返回，本次未把它们计作生产库回放证据。

| 项目 | 证据 |
| --- | --- |
| 历史/查询一致性 | 原有 34 项 store 回归；新增操作分组、缺失基线、CLI 默认区间 |
| 配置 | 旧字段默认值、正常保存、替换失败保留旧配置、清理临时写入文件 |
| 预览 | 中文/空格、年龄精确边界、递归、重叠去重、卷不符、根/保护目录、取消、过期/修订冲突 |
| Windows 文件操作 | 专用临时普通文件、预览后新文件、身份替换、父联接替换、长路径、祖先固定后禁止重命名 |
| 特殊/不可访问文件 | 多硬链接与预览后新增硬链接、只读、共享占用、ACL 拒绝；执行器不改 ACL |
| 执行恢复 | 取消只停止后续项、pending 转 unknown、不重放、每日声明跨修订保留、200 条明细及 30 天裁剪 |
| CLI 真调用 | UTF-8 文件及 stdin、中文空格、单个 JSON、空页、total/offset、错误码、修订/过期、执行与重试 |
| CLI 真中断 | 仅终止新启动的临时样本子进程，status 推断 interrupted，重试返回原 run 且存活文件数不变 |
| 核验策略 | RequireIncremental 拒绝全量的策略回归、取消在打开卷之前退出、原有游标/提交回滚和日志断档回归 |
| 并发 | 现有全局锁与分盘锁回归；所有写入口共用全局锁 |

所有删除、联接和 ACL 样本均由本次测试创建在任务临时目录。没有对真实用户目录创建或启用规则。

## 可复核的视觉证据

`ui-capture` 是默认关闭的开发特性。它在隐藏、移到屏幕外的真实 Windows/OpenGL 窗口中渲染
实际应用 UI，读取 framebuffer 保存 PNG，并自动退出。没有接管桌面或发送用户输入。

本地 `evidence/ui/`：overview-1050、overview-760、preview-1050、preview-760-125、
result-1050、history-1050、error-760、empty-760。1050×720 / 760×520 是逻辑窗口尺寸；本机 Windows DPI
实际为 175%，物理截图相应约 1838×1260 / 1330×910。`125` 表示额外 125% 应用缩放，未改系统缩放。
人工阅读截图检查了中文、长目录、布局、增减符号、空/错误与核验未完成提示；窄窗口通过纵向滚动容纳表单。

这些图片只证明实际渲染，不等于真实点击、键盘、滚动、焦点或完整用户操作验收。
复现：`cargo build --features ui-capture --example ui_capture`，然后运行
`ui_capture OUTPUT.png overview|preview|result|history|error|empty WIDTH HEIGHT SCALE`。
result 场景只删除它自己刚创建的两个 fixture 文件；scope 和数据库均在输出目录内。

## 性能样本（调试构建，同机同样本）

命令：`cargo build --example cleanup_bench`，分别运行 baseline / no-rules / preview / execute。
原始 JSON 在本地 `evidence/performance/review-*.json`，每个进程独立创建专用样本。

| 样本 | 耗时 | 进程峰值工作集 | 数据库变化 |
| --- | --- | --- | --- |
| 2 万索引文件，30 次增量提交，对照 | 120.16 ms | 23.79 MiB | 物理文件合计 -10.65 MiB；逻辑页 -2.80 MiB |
| 同样本，加无规则编排 | 131.81 ms | 23.79 MiB | 与对照相同 |
| 指定范围 1000 个 4 KiB 文件，预览 | 1639.43 ms | 17.95 MiB | 逻辑页 +960 KiB；物理文件 +325.90 KiB |
| 同样本，预览后执行 | 1557.25 + 3314.19 ms | 18.91 MiB | 逻辑页 +1000 KiB；含 WAL 物理文件 +5.07 MiB |

对照仅比较 SQLite 增量提交/整理与额外无规则编排，约增加 0.39 ms/次；早前两组为对照
115.57–127.33 ms、无规则 123.09–125.91 ms。负数据库增量来自 staging 空页回收/WAL 变化。
这是局部样本，**不是原始 USN 扫描整体性能结论**，未测清理后真实增量核验成本。

## 剩余验收与阻塞

1. 原生交互驱动：当前仅浏览器 CUA，原生禁用；computer-use 要求的 node_repl 未暴露。
   待主代理协调驱动后，真实走通看变化→目录→规则→预览→明确确认→启用/执行→结果，检查键盘、滚动、忙碌、缩放及错误恢复。
2. 原始 NTFS/USN：本地账户 probe E 返回拒绝访问；CI d4897f9 专用 VHD 已实际通过增量核验、删除只计一次、日志重置拒绝全量及游标保持。
   最新迁移修复与自动/手动/同日/取消场景需对应提交的 CI；未创建或修改真实 Windows 登录任务。
3. 云占位/离线等属性已有拒绝代码，但本次没有真实云提供商占位文件样本；需进一步 Windows 环境验收。
4. 外部生产历史库回放未运行；已完成合成旧模式迁移与重复打开回归。需要时只在隔离副本运行现有 production_* 入口。
5. 远端分支推送已在具体用户授权后成功；仍不合并 main、不发布 Release。

本次候选包只供最终审查；尚未达到批准计划第四阶段的全部验收门槛。


## 审查截图与运行限制

三张更新截图使用人工显示目录 `E:\VolumeTrail Samples\临时转储`，不包含宿主用户路径。
Library：概览 `libfile_96564280855081918789c3adecf8c18f`；窄屏预览 `libfile_5b3756346bf08191bfad73a6323fc0df`；结果 `libfile_b1903ea53db48191bf0954b96335385d`。
它们来自实际原生渲染的合成数据模型，仍不证明真实点击/键盘/滚动。

本机 CUA 原生禁用，未暴露 node_repl/@oai/sky。终端 require_escalated 是工具沙箱权限，不会自动提供 Windows 管理员令牌。
现有应用 runas 路径会请求 UAC，只用于扫描；本次未调用，也未修改安全设置或登录任务。
CI 专用卷流程、精确覆盖与不覆盖项见 [ci-ntfs-validation.md](ci-ntfs-validation.md)。

## 本次候选版本迁移补验

旧 v9 首项 pending 已持久化、但汇总仍为 deleted=0/unknown=0/not_needed 的情形，迁移与恢复均结合未决明细保护增长基线。
回归模拟真实样本删除后中断，再迁移、恢复、扫描；下一普通增长区间保持未知，避免混入不确定清理。

已读取 CI [39583001680](https://github.com/BW1145/VolumeTrail/actions/runs/39583001680) 的 runtime.json，
status=passed、isolated_vhd=true，full_ms=1437、incremental_ms=507、cleanup_and_verify_ms=518。
证据位于本地 evidence/ntfs-ci-39583001680；这些耗时来自 CI 隔离卷，不能与本地 SQLite 样本直接做前后性能比较。

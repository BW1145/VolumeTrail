# 隔离 NTFS/USN 实机验证

入口为 `tests/run-isolated-ntfs.ps1`，仅允许临时 GitHub-hosted Windows VM。
脚本检查 `GITHUB_ACTIONS`、`RUNNER_ENVIRONMENT` 和实际管理员令牌；缺少权限时写出 not_run 并失败，不能算通过。
本地未调用该脚本，也未操作宿主磁盘日志或现有登录任务。

GitHub 的[托管 runner 文档](https://docs.github.com/en/actions/reference/runners/github-hosted-runners#administrative-privileges)
说明 Windows VM 默认以管理员运行；测试仍检查实际令牌和能力，不依靠标签推断成功。

脚本在 RUNNER_TEMP 内创建唯一目录及 256 MiB 动态 VHD。diskpart 只按这个字面文件路径选择 VHD，
不会选择宿主磁盘编号。格式化后反查分区对应的 DiskImage，要求镜像路径完全相同、文件系统为 NTFS、
卷标为 VolumeTrailCI，再创建随机标记和 USN 日志。R–Z 中选择空闲盘符。
数据库位于本次 runner 临时目录；规则只匹配 VHD 中新建 Samples 目录的一份 dmp 文件。

测试精确覆盖：

- 在专用 NTFS 卷完成首次全量扫描与真实 USN 增量扫描，并核对 Windows 文件身份与扫描器卷身份一致。
- 通过实际 cleanup worker 删除一份专用样本，RequireIncremental 核验成功，原扫描/核验点关联正确。
- 文件夹删除增量等于该候选占用；再一次 USN 增量中样本目录变化为零，删除只计一次。
- 空数据库 RequireIncremental 拒绝首次全量，并保持无报告。
- 仅在该 VHD 使用 `fsutil usn deletejournal /n` 并重新创建日志；要求核验拒绝全量回退，数据库游标与报告数保持不变。
- 真实便携 CLI scan-auto 完成清理和增量核验；同日第二次自动扫描不会重复清理，手动 scan 保留新匹配文件。
- 独立数据库首次自动扫描在忙碌状态收到取消信号后失败；不生成清理任务，样本文件保留。不注册登录任务。
- 记录全量、增量、清理加核验耗时与断言结果到 `evidence/ntfs-ci/runtime.json`。

`/n` 的同步等待语义见 [Microsoft fsutil usn](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/fsutil-usn)。
日志禁用只允许经过路径和随机标记验证的本次 VHD。finally 按同一镜像文件路径卸载，并仅在确认未挂载且仍在本次临时目录内时删除镜像文件。

默认 `cargo test` 对这个入口显示 ignored；CI 另行明确运行 `--test ntfs_runtime -- --ignored --nocapture`。
workflow 在 main、开发分支和 PR 上构建，上传单独的 NTFS 证据，即使失败也上传 preflight/创建/卸载日志。
存在 runtime.json 且 status=passed，加上该 job 成功，才构成本项通过证据。

尚不由这个测试证明：真实 Windows 桌面点击/键盘/滚动、真实登录任务触发、宿主卷换盘、云提供商占位、
生产历史库回放和自然日志覆盖断档。日志覆盖断档继续使用现有模拟策略回归；不得为补覆盖而修改宿主日志。

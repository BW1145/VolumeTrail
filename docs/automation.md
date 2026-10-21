# VolumeTrail 自动化接口

`VolumeTrail-cli.exe capabilities --json` 返回版本、命令、权限、限额和退出状态。
`--json` 模式 stdout 只有一个 UTF-8 JSON 对象；进度在 stderr。成功对象有
`schema_version: 1`、`ok: true`、`status`、`command`；失败有 `ok: false` 和
`error.code/message`。保留旧 query 命令与文本默认输出，版本 1 为加法兼容契约。

## 查询

```powershell
VolumeTrail-cli.exe query overview --drive C --json
VolumeTrail-cli.exe query growth --drive C --limit 20 --offset 0 --json
VolumeTrail-cli.exe query folder --drive C --path 'C:\Samples' --from 10 --to 12 --json
VolumeTrail-cli.exe query largest --drive C --kind files --limit 50 --offset 50 --json
```

query 只读现有历史，不自动扫描、提权或清理。没有历史时返回明确空状态或失败。
overview 与 GUI 共用服务；`current_index` 是最近成功索引的容量与观测时间，`latest` 是
仍保留的最近历史点，两者可能因用户删历史而不同。`interval` 是增长基线，缺失时为 null。
默认增长比较为“上次操作最终成功点 → 本次清理前点”，不会取几秒内的核验差值。
显式 from/to 指物理扫描 ID；覆盖元组顺序为总区间、完整目录汇总、可用汇总、逐项明细区间。
覆盖不足的数据不能当作完整变化。summary/growth/folder/largest 返回 total/has_more；
folder 的 comparison.children 也应用 offset，扩展名排行带 total/truncated。

## 建规则、预览和授权

将规则以 UTF-8 JSON 文件或 stdin 传入，无需命令行转义路径与通配符：

```json
{
  "name": "我指定的转储文件",
  "directory": "E:\\专用样本\\临时转储",
  "patterns": ["*.dmp", "*.tmp"],
  "recursive": false,
  "min_age_days": 7
}
```

```powershell
VolumeTrail-cli.exe cleanup save --input rule.json --json
VolumeTrail-cli.exe cleanup preview --id RULE_ID --json
```

保存时读取并绑定卷身份，返回 `rule.id`、`revision`、`volume_id`。保存始终暂停；
编辑时提交完整规则并保留当前 id/revision，过期修订返回 revision_conflict，不能覆盖新设置。
年龄 0 表示不按年龄保留，默认不递归。只支持文件名 `*`/`?`，多个模式放数组。
计划包含规则快照、卷、目录范围、永久删除方式、候选身份、数量、占用估算、跳过原因和截断标记，
15 分钟有效。一次执行只能重新验证这份计划里的对象，不能增加预览后出现的文件。
自动规则按已启用范围重新规划，同一规则每天最多自动运行一次。

AI 必须先向用户说明具体目录、匹配、递归、年龄与直接删除性质，取得明确授权，才能执行或启用：

```powershell
VolumeTrail-cli.exe cleanup execute --plan PLAN_ID --request UNIQUE_REQUEST_ID --confirm-permanent-delete --json
VolumeTrail-cli.exe cleanup enable --id RULE_ID --revision N --plan PLAN_ID --confirm-automatic-rule --json
VolumeTrail-cli.exe cleanup pause --id RULE_ID --revision N --json
VolumeTrail-cli.exe cleanup delete --id RULE_ID --revision N --json
```

编号、命令访问权和确认参数不代表用户已经授权。启用需新预览且未截断；范围修改后必须再保存、预览和启用。
删除规则只删配置。直接删除文件不可撤销，不备份、不进回收站。
只处理单硬链接普通文件；系统/程序/自身目录、重解析点、云占位、离线、只读、系统文件均不处理。
权限或共享冲突跳过，不提权补救。卷身份变化拒绝执行。

## 结果、重试和取消

```powershell
VolumeTrail-cli.exe cleanup status --id RUN_ID --json
VolumeTrail-cli.exe cleanup runs --drive C --limit 20 --offset 0 --json
VolumeTrail-cli.exe cleanup cancel --id RUN_ID --json
```

同一 request_id 重试返回原 run_id。不同计划不能复用请求 ID。取消只停止后续文件。
`state` 区分 completed/partial/cancelled/interrupted/running；`verification` 独立区分
verified/not_needed/pending/unverified/cancelled/verified_by_later_scan。
删除成功但核验未完成时，索引待下次扫描更新；不会回退到全量核验。
`allocated_bytes_estimate` 是已删除文件占用估算，`free_after-free_before` 是磁盘实际可用空间变化，
可能包含其他进程并发写入，不能全部归功于清理。真实观测时间保留。

pending 行在删除前持久化。异常退出不重放，标记 unknown 并等待扫描核验。
只读 status 在无操作锁时可推断 interrupted；实际恢复由持锁的下次操作写回。
逐项结果最多 200 条，30 天后裁剪并标记；汇总/request 幂等保留最多 1000 次且最多 365 天。
计划最多保留 32 份。超出保留期的请求没有幂等保证，不能盲目重试旧删除请求。

退出码：0 成功/查询成功；1 失败；2 部分完成或核验未完成；3 取消；4 修订/计划冲突或中断。
status 返回该次执行对应退出状态，runs 列表查询成功为 0。

登录仍使用原 scan-auto 生命周期；成功扫描后执行已启用规则，沿用同一全局锁，最终退出。
手动 `scan` 只扫描。安装或移除登录任务不属于规则配置过程。

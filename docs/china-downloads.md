# 中国地区下载策略

覆盖已开放的 Windows x64 软件插件。镜像只改变传输路径，不改变受管安装的来源 owner，
也不替代官方签名、SHA-256、健康检查或原子提交。

## 地区与回退

默认根据 `LC_ALL`、`LC_MESSAGES`、`LANG`、`LANGUAGE` 和 Windows 用户 `LocaleName`
识别 `zh-CN`。启动进程可用 `TORBEN_REGION=CN` 强制国内候选，或
`TORBEN_REGION=global` 使用原始来源；覆盖优先于系统语言，不修改系统环境或查询 IP。

连接超时 10 秒，读取停滞超时 20 秒。安装包传输每 15 秒检查速度，低于 16 KiB/s
时切换候选。HTTP 错误、下载中断、大小或哈希不符会回退，原始安装包地址最后重试一次。
取消会停止等待；磁盘、权限等本地错误直接报告。只接受预定义的精确 HTTPS 镜像资源。
MySQL 保留单一来源内的断点续传，切换来源时清除残片。
取消丢弃请求后仍执行残片清理；文件在请求让出执行前打开，避免后台创建晚于回滚完成。

## 来源与校验

| 插件 | 中国地区传输候选 | 校验依据 |
| --- | --- | --- |
| Node.js | 清华 `nodejs-release` → 华为 `nodejs` → 官方；版本索引仅用官方 | 官方签名 checksum 清单，清单与签名从同一候选读取 |
| Java / Temurin | 清华 `Adoptium` → 南京大学 `adoptium` → GitHub；GitHub 故障时尝试官方 asset API | Adoptium API 的大小、SHA-256 和原有发布签名 |
| Python | 华为 `python` → 南京大学 `python` → 官方 | 官方 Windows 分页索引的精确版本、架构与 SHA-256 |
| Rust | 清华 `rustup/dist` → 中科大 `rust-static/dist` → 官方 | 原始官方 channel manifest 的 SHA-256 |
| MySQL | 华为 `mirrors.huaweicloud.com/mysql/Downloads` → `repo.huaweicloud.com/mysql/Downloads` → 官方 | Core 固定的 Windows ZIP SHA-256 |
| Redis | GitHub 官方 asset API → release 地址 | 精确 asset 匹配及 Core 固定的社区 Windows ZIP SHA-256 |
| PostgreSQL | EDB 官方地址，增加超时、低速检测与重试 | Core 按 WinGet 清单固定的 installer SHA-256 |

Node.js 镜像索引曾返回 HTTP 200 但落后官方，因此不用于发现版本。Rust 仅镜像安装包，
避免镜像 manifest 改写 URL/hash；Windows 使用官方 `tar.gz` 并合并组件，保留工具链和文档，
避免 MSI 管理安装的耗时。Java detached signature 也可经官方 asset API 获取。

Python 元数据每次请求总超时 6 秒、网络失败重试一次，整个版本查询最多 25 秒，
在插件 30 秒 RPC 限制内返回错误。Windows 发布 API 不可用时回退独立官方分页索引，
仍校验版本、架构、URL 和 hash；解析或来源错误不会触发绕过。现代 ZIP 由 Core 下载校验后，
内置 Python Install Manager 使用本地索引提取。无官方 SHA-256 的旧 NuGet 包保留
manager 官方下载路径，不使用第三方 hash。

## 安装异常与升级

失败只回滚当前任务，错误保留在对应插件页面；切换后返回仍能查看、重试。
进度只展示在版本行。首次且唯一的受管版本会等待其他安装释放锁后设置主版本，
已有选择保持不变；自动选择失败记录 `selection_skipped`，不把已提交安装误报为失败。
卸载删除与锁等待在后台执行。Java 暂存位于受管库旁，保证跨盘自定义库仍可原子 rename；
Windows 短暂占用时有限重试，永久错误保留系统错误码与源、目标路径。

替换 `TorbenApp.exe` 不会自动更新已安装的内置插件。要使用新 Provider，需通过插件页
卸载旧插件再启用；已有受管运行时或主版本时卸载保护仍生效，应先处理对应运行时及引用。
数据库实例会阻止卸载其绑定版本，请先备份并明确处理实例。不可手工覆盖插件文件或删库。

## 验证边界

2026-10-08 经用户授权，在本机下载七个精确资源，合计约 1.18 GiB；大小与 SHA-256
全部匹配官方签名清单、官方元数据或 Core 固定值，Temurin detached signature 独立验证通过。

| 版本 | 实际来源 | 平均速度（MiB/s） |
| --- | --- | ---: |
| Node.js 24.21.0 | 华为 | 10.17 |
| Temurin 21.0.12+101.0.LTS | 清华 | 10.76 |
| Python 3.14.7 | 华为 | 10.34 |
| Rust 1.98.0（旧 MSI） | 中科大 | 6.02 |
| MySQL 8.4.11 | 官方 CDN | 6.16 |
| Redis 8.8.0 | GitHub release | 6.07 |
| PostgreSQL 18.6-3 | EDB 官方 | 6.01 |

这是历史下载与完整性证据，未执行安装；Rust 新 tarball 尚无完整下载测速。
速度不代表其他网络、时间或版本。普通测试用本地 fixture 覆盖候选、低速切换、哈希回退、
取消、Python 本地索引、并发选择、Java 文件锁和回滚，不能代替真实 GUI 安装验收。

当日华为三个固定 MySQL 版本均缺少可用镜像；Redis 社区 Windows 包和 EDB Windows
installer 尚无已确认的可靠国内镜像。这些路径可能仍回退官方，已有 HTTP(S) 代理由
reqwest 使用。镜像目录存在或 HEAD 200 不证明版本可用、完整性或速度。

参考：[清华 Rustup](https://mirrors.tuna.tsinghua.edu.cn/help/rustup/)、
[Python Windows 索引](https://www.python.org/ftp/python/index-windows.json)、
[Python Install Manager](https://docs.python.org/3/using/windows.html)、
[GitHub asset API](https://docs.github.com/en/rest/releases/assets#get-a-release-asset)。

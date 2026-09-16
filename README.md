# Torben App

Torben App 是本地优先的 Windows x64 应用与运行时管理器。当前版本以单文件
`TorbenApp.exe` 交付，通过插件逐个开放软件管理能力；目前支持 Java（Eclipse Temurin）、Python、Node.js、Rust、MySQL、Redis 和 PostgreSQL。

项目为独立重写，不读取或复用 SoftPilot 的代码与状态格式。

## 当前范围

- 支持平台：Windows x64。
- 当前软件插件：Java（Eclipse Temurin）、Python、Node.js、Rust、MySQL、Redis 和 PostgreSQL。
- 暂不开放：Git、Visual Studio Code 和 Codex CLI。
- 暂缓平台：Windows ARM64、macOS 和 Linux。
- 不包含账号、云同步、遥测、常驻后台服务和项目级版本固定。

未开放的软件仍保留 Core、Provider 和本地 fixture 实现，待确认其自身数据能够限制在
Torben App 数据目录后再逐个恢复。

## 使用

便携包只有一个文件：

```text
TorbenApp.exe
```

首次启动时，Torben App 按 D 到 Z 查找第一个可用盘符，并建议
`<盘符>:\TorbenApp`。用户可确认或选择其他基准目录。确认后形成：

```text
<基准目录>\
├─ TorbenApp.exe
└─ userData\
```

`userData` 保存数据库、配置、插件、受管软件、缓存、日志、操作记录、命令 shim 和
WebView2 数据。升级只需运行新的 `TorbenApp.exe`；应用会替换目标目录中的旧版本，保留
`userData`，重新启动目标程序，并清理内容一致的原始启动文件。

Release 可执行文件通过 Windows manifest 请求管理员权限。开发构建不会在每次启动时触发
UAC。

界面默认打开“插件”。安装 Java、Python、Node.js、Rust、MySQL、Redis 或 PostgreSQL 插件后，左侧出现对应的管理入口；设置
固定在侧栏底部，日志和诊断集中在标题栏“帮助”菜单中。“帮助”菜单也提供版本和产品
介绍。Python 插件内置固定版本且经过 SHA-256 校验的 python.org 官方 Python Install
Manager；Torben App 使用其 `--target` 模式将运行时写入 `userData`，不依赖系统 `PATH`
中的 `py`。

插件卡片可通过左侧拖拽手柄直接调整顺序，排序保存在 Core 用户设置中，并同步用于左侧已安装插件菜单。
Node.js、Java、Python 和 Rust 的插件详情页支持配置进程级环境变量；这些变量只会应用于之后通过 Torben shim
启动的对应命令及其子进程，不修改 Windows 用户或系统环境变量。Torben 管理的 `PATH`、包缓存和数据目录变量
不可覆盖，密码、Token 等敏感值不应保存在环境变量设置中。

Node.js 插件提供官方 LTS、Current 和精确版本安装，以及 `node`、`npm`、`npx`、`pnpm` 终端选择。
通过 Torben shim 启动时，npm 缓存、全局包、配置、临时文件和 Node.js 历史默认保存在
`userData/package-managers/node/npm`，切换或卸载 Node.js 版本会保留 npm 数据。pnpm 的 store、状态和缓存位于
`userData/package-managers/node/pnpm`。项目依赖仍写入项目目录；
显式命令行路径选项和用户运行的脚本可以选择其他位置，这不是文件系统沙箱。
全局包可以通过 `npm exec --global -- <命令>` 调用，无需增加系统 PATH 条目。
pnpm 本身通过受管 npm 全局前缀安装，例如 `npm install --global pnpm@11.19.0`；其可执行文件位于
npm 的全局目录，pnpm store、状态和缓存则位于独立的 `userData/package-managers/node/pnpm`。

Rust 插件使用官方 Rust stable 发行版，为 Windows x64 安装 `rustc`、`cargo`、`rustdoc` 和
`rustfmt`。每个版本独立安装并可设置主版本；Cargo registry/git 缓存和临时目录统一保存在
`userData/package-managers/rust/cargo`，项目的 `target` 与 `Cargo.lock` 仍由项目自身管理。界面默认展示最近
3 个稳定版本，但仍支持按精确版本安装。

MySQL 插件管理官方 MySQL Community Server Windows x64 ZIP，并提供 `mysql`、`mysqld`、
`mysqladmin` 和 `mysqldump`。Redis 官方不提供原生 Windows Open Source 二进制，因此 Redis
插件明确使用 `redis-windows` 社区项目从上游 Redis 源码构建的 Windows x64 ZIP，提供
`redis-server`、`redis-cli` 和 `redis-benchmark`。两者均支持多版本并存和主版本切换；
客户端历史分别保存在 `userData/application-data/mysql/client` 和
`userData/application-data/redis/client`。桌面端和 `torben instance` CLI 通过同一套 Core API
创建、启动、停止、检查、备份、恢复和删除实例。每个实例固定绑定创建时的运行时版本，数据、配置、日志、临时文件和
内部备份位于 `userData/application-data/<engine>/instances/<name>`；插件卸载和普通运行时升级不会删除实例数据，
仍被实例引用的运行时版本不能卸载。MySQL 提供 8.4.6（LTS，推荐）、8.0.46 和 5.7.44。

MySQL、Redis 和 PostgreSQL 的管理页分为“版本管理”和“实例管理”两个页签：前者负责运行时安装、选择和卸载，
后者负责实例创建、启动、停止、状态检查、备份、恢复和删除。

PostgreSQL 插件提供 EDB 分发的 Windows x64 PostgreSQL server 和命令行工具。Torben App
按 Microsoft WinGet 清单固定 installer SHA-256，只调用 EDB installer 的 `extract-only`
模式，不注册 Windows 服务、不安装 pgAdmin 或 StackBuilder，也不会在安装运行时期间执行 `initdb`。18 和
17 major 可并行安装并分别设为主版本；PostgreSQL 配置与凭据文件路径统一位于
`userData/application-data/postgresql/client`。安装或切换运行时不会自动设置共享 `PGDATA` 或创建 cluster；
只有用户显式执行“创建实例”时，Core 才会在该实例目录运行 `initdb`，并把 cluster 固定到所选 major 版本。

实例 CLI 示例：`torben instance list mysql`、`torben instance create mysql local --version 8.4.6 --port 3306`、
`torben instance start mysql local`、`torben instance backup mysql local`、
`torben instance restore mysql local D:\\backups\\local.sql`、`torben instance stop mysql local` 和
`torben instance delete mysql local --confirm`。显式备份与恢复路径必须是绝对路径，已有备份文件不会被覆盖。

Python 的 `pip` 也通过 Torben shim 启动。pip 缓存、`--user` 安装目录、配置和临时文件
默认保存在 `userData/package-managers/python/pip`，不会写入用户配置目录。项目虚拟环境和项目依赖仍属于项目，
由项目自身的锁文件管理。当前插件集合没有 Maven 或 Gradle 插件；Java 插件只提供 JDK，
安装计划不会启动 Maven、Gradle 或其他包管理器。

## 仓库结构

- `apps/desktop`：Tauri 2 与 React 桌面端。
- `crates/torben-contracts`：共享模型和插件协议。
- `crates/torben-core`：路径、状态、事务和软件管理。
- `crates/torben-plugin-host`：原生插件 JSON-RPC host。
- `crates/torben-cli`：`torben` CLI。
- `crates/torben-shim`：受管命令转发 shim。
- `plugins/*`：第一方软件 Provider。
- `packages/ui`：桌面端设计系统。
- `eng`：构建、验证和发布脚本。

## 开发环境

- Rust stable 1.98.0，Windows 使用 MSVC toolchain。
- Node.js 24.19.0 LTS。
- pnpm 11.19.0。
- [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/)。

依赖已存在时，常用检查命令为：

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
pnpm run check
pnpm run test
```

启动桌面开发环境：

```powershell
pnpm dev
```

构建 Windows x64 单文件便携包：

```powershell
pnpm run prepare:python-manager
pnpm --filter @torben-app/desktop tauri:build:portable
```

第一条命令会从 `python.org/ftp/python/pymanager` 下载固定的 Python Install Manager 26.3
MSI 到忽略版本控制的 `.tools` 目录，并校验发布页公布的 SHA-256。构建脚本本身不会隐式
下载，缺少该文件时会直接失败并给出准备命令。

产物位于：

```text
artifacts\torben-app-portable-windows-x64\TorbenApp.exe
```

## 架构约束

- GUI 与 CLI 调用同一组 Core API，并共享操作状态和错误码。
- 插件仅通过版本化 JSON-RPC over stdio 与 host 通信，不直接访问 Core 数据库。
- 原生插件属于受信任代码；进程边界用于故障隔离，不是安全沙箱。
- 一个受管安装只有一个不可变来源；切换来源必须执行明确的卸载/重装迁移。
- 安装顺序固定为下载、验证、暂存、健康检查、原子文件提交、状态提交。
- 普通测试使用本地 fixture；真实官方元数据检查只在显式 CI 任务中运行。

## 进一步文档

- [架构](docs/architecture.md)
- [Windows 里程碑状态](docs/milestone-status.md)
- [测试与验收](docs/testing.md)
- [发布工程](docs/release.md)
- [插件注册表发布](docs/plugin-registry-publishing.md)

## License

Licensed under the Apache License 2.0.

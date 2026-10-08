# 软件与数据库实例管理

当前支持 Windows x64。Java（Eclipse Temurin）、Python、Node.js、Rust、MySQL、Redis 和
PostgreSQL 插件安装后才开放对应管理入口。安装、终端选择和卸载均通过共享 Core API 执行。

## 运行时与数据目录

Node.js 插件提供官方 LTS、Current 和精确版本安装，以及 `node`、`npm`、`npx`、`pnpm` 终端选择。
通过 Torben shim 启动时，npm 缓存、全局包、配置、临时文件和历史默认保存在
`userData/package-managers/node/npm`，pnpm 的 store、状态和缓存保存在
`userData/package-managers/node/pnpm`；切换或卸载 Node.js 版本会保留这些数据。
项目依赖仍由项目自身管理，显式路径和用户脚本可以选择其他位置。全局包可用
`npm exec --global -- <命令>` 调用；pnpm 通过受管 npm 全局前缀安装，例如
`npm install --global pnpm@11.19.0`，无需增加系统 PATH 条目。

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
仍被实例引用的运行时版本不能卸载。MySQL 提供 8.4.11（LTS，推荐）、8.0.46 和 5.7.44。

MySQL、Redis 和 PostgreSQL 的管理页分为“版本管理”和“实例管理”两个页签：前者负责运行时安装、选择和卸载，
后者负责实例创建、启动、停止、状态检查、备份、恢复和删除。

PostgreSQL 插件提供 EDB 分发的 Windows x64 PostgreSQL server 和命令行工具。Torben App
按 Microsoft WinGet 清单固定 installer SHA-256，只调用 EDB installer 的 `extract-only`
模式，不注册 Windows 服务、不安装 pgAdmin 或 StackBuilder，也不会在安装运行时期间执行 `initdb`。18 和
17 major 可并行安装并分别设为主版本；PostgreSQL 配置与凭据文件路径统一位于
`userData/application-data/postgresql/client`。安装或切换运行时不会自动设置共享 `PGDATA` 或创建 cluster；
只有用户显式执行“创建实例”时，Core 才会在该实例目录运行 `initdb`，并把 cluster 固定到所选 major 版本。

实例 CLI 示例：`torben instance list mysql`、`torben instance create mysql local --version 8.4.11 --port 3306`、
`torben instance start mysql local`、`torben instance backup mysql local`、
`torben instance restore mysql local D:\backups\local.sql`、`torben instance stop mysql local` 和
`torben instance delete mysql local --confirm`。显式备份与恢复路径必须是绝对路径，已有备份文件不会被覆盖。

Python 的 `pip` 也通过 Torben shim 启动。pip 缓存、`--user` 安装目录、配置和临时文件
默认保存在 `userData/package-managers/python/pip`，不会写入用户配置目录。项目虚拟环境和项目依赖仍属于项目，
由项目自身的锁文件管理。当前插件集合没有 Maven 或 Gradle 插件；Java 插件只提供 JDK，
安装计划不会启动 Maven、Gradle 或其他包管理器。

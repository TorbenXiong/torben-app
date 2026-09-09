# Torben App

Torben App 是本地优先的 Windows x64 应用与运行时管理器。当前版本以单文件
`TorbenApp.exe` 交付，通过插件逐个开放软件管理能力；目前支持 Java（Eclipse Temurin）和 Python。

项目为独立重写，不读取或复用 SoftPilot 的代码与状态格式。

## 当前范围

- 支持平台：Windows x64。
- 当前软件插件：Java（Eclipse Temurin）和 Python。
- 暂不开放：Node.js、Git、Visual Studio Code 和 Codex CLI。
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

界面默认打开“插件”。安装 Java 或 Python 插件后，左侧出现对应的运行时管理入口；设置
固定在侧栏底部，日志和诊断集中在标题栏“帮助”菜单中。“帮助”菜单也提供版本和产品
介绍。Python 插件内置固定版本且经过 SHA-256 校验的 python.org 官方 Python Install
Manager；Torben App 使用其 `--target` 模式将运行时写入 `userData`，不依赖系统 `PATH`
中的 `py`。

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

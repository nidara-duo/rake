# <img src="../assets/rake_logo.ico" width="42" height="42" valign="middle" alt="Rake Logo"> Rake（Rust 重写的 Scoop）

[![版本](https://img.shields.io/badge/version-0.1.0-blue?style=for-the-badge)](https://github.com/nidara-duo/rake/releases)
[![许可证](https://img.shields.io/badge/license-GPL--3.0-green?style=for-the-badge)](LICENSE)
[![平台 Windows](https://img.shields.io/badge/platform-windows-0078d7?style=for-the-badge&logo=windows)](https://www.microsoft.com/windows)
[![语言 Rust](https://img.shields.io/badge/language-Rust-ea4aaa?style=for-the-badge&logo=rust)](https://www.rust-lang.org)

<div align="center">

[🇬🇧 English](../readme.md) | 🇨🇳 简体中文

</div>

**Rake** 是一款完全以 Rust 语言从零实现的 Windows 包管理器，兼容流行的 **Scoop** 包管理器。

作为原始 PowerShell 版 Scoop 的即插即用替代品，Rake 拥有极快的速度和轻量级的体积，完全独立于原始代码库，同时保持 **100% 兼容** 现有的环境、应用、清单（manifest）和软件源（bucket）。

---

## ⚡ 性能对比（Rake vs. Scoop）

得益于 Rust 的原生编译和高度优化的 I/O 操作，Rake 执行命令的速度比 Scoop 快 **数十到数百倍**。

| 命令 | Scoop（平均） | Rake（平均） | 提速 |
| :--- | :--- | :--- | :--- |
| `list`（列出已安装应用） | ~582.90 ms | **~19.80 ms** | **~30 倍** |
| `status`（检查更新） | ~4341.90 ms | **~28.10 ms** | **~154 倍** |
| `search rustup`（搜索软件源） | ~2531.05 ms | **~282.55 ms** | **~9 倍** |
| `bucket list`（列出存储库） | ~400.40 ms | **~196.00 ms** | **~2 倍** |
| `bucket add nonportable` | ~38063.15 ms | **~102.25 ms** | **~372 倍** |
| `bucket rm nonportable` | ~102.90 ms | **~12.75 ms** | **~8 倍** |

*注意：* `checkup` *命令的优化尚在开发中（Scoop：~75ms，Rake：~908ms）。*

---

## ✨ 功能特性

- **极速性能** — 告别冷启动 PowerShell 的漫长等待。
- **纯自主运行** — 仅需单个独立的 `rake.exe` 二进制文件，日常任务无需启动庞大的 PowerShell 会话。
- **无缝兼容 Scoop** — 开箱即用，自动继承现有 Scoop 的安装路径、配置、本地软件源和下载缓存。
- **优雅解耦** — 包管理逻辑位于核心引擎中，而自更新和复杂安装操作则通过轻量级的引导机制完成，互不干扰。
- **默认安全** — 对所有远程资源和内部发布版本实施严格的 SHA-256 校验。

---

## ⚙️ 架构设计

项目由一组高度解耦的模块化 crate 组成，以确保代码的可维护性和可测试性：

- `rake-domain` — 纯领域类型、清单解析和严格的校验规则（完全独立于 I/O 和网络层）。
- `rake-core` — 核心中枢，负责异步执行、状态管理、包操作和内部事件总线。
- `rake-cli` — 终端前端，使用 `Clap` 进行参数解析，结合 `Indicatif` 提供美观的交互式进度条。
- `rake-hash` — 高效的文件哈希计算后端。
- `rake-shim-bin` — 超轻量级的 Windows 可执行文件模板，用于低开销的应用 shim。

---

## 🚀 安装方法

### 方法一：自动引导脚本（推荐）

通过一条 PowerShell 命令即可安装 Rake。脚本会自动检测您的 CPU 架构（`x86_64`、`i686`、`aarch64`），下载最新发布版本，校验哈希值，并更新系统 `PATH` 环境变量：

```powershell
powershell -ExecutionPolicy Bypass -Command "iwr -useb https://raw.githubusercontent.com/nidara-duo/rake/main/scripts/bootstrap.ps1 | iex"
```

### 方法二：从源码手动构建

如果您希望自行编译，请确保已安装 Rust 工具链：

克隆仓库：

```bash
git clone https://github.com/nidara-duo/rake.git
cd rake
```

编译发布版本：

```bash
cargo build --release
```

编译后的二进制文件位于 `target/release/rake.exe`。

---

## 🔄 自我更新

Rake 将包管理与自身二进制文件的更新清晰分离。要安全地将 `rake.exe` 更新至最新稳定版（无需担心文件占用锁定），只需运行：

```powershell
rake self update
```

该命令将触发后台引导程序，自动获取、校验并干净地替换当前可执行文件。

---

## 🛠️ 可用命令

Rake 提供了一套完整的 CLI 命令，完全覆盖 Scoop 的所有工作流程：

```plaintext
Commands:
  alias       管理 Scoop 别名
  bucket      管理软件源（bucket）
  cache       管理下载缓存
  cat         查看指定清单内容
  checkup     检查潜在问题
  cleanup     清理包的旧版本
  config      获取或设置配置值
  download    将包下载到缓存
  export      以 JSON 格式导出已安装的应用、软件源和配置
  hold        锁定包以禁止更改
  home        打开包的主页
  import      从 JSON 格式的 Scoopfile 导入应用、软件源和配置
  info        查看包的基本信息
  install     安装应用
  list        列出已安装的应用
  reset       重置应用以解决冲突
  search      搜索可用包
  self        管理 Rake 自身（安装、更新、卸载）
  shim        操作 Scoop 的 shim
  status      查看状态并检查应用的新版本
  unhold      解锁包以允许更改
  uninstall   卸载应用
  update      将已安装的包更新到最新版本
  which       定位 shim 或可执行文件
```

---

## 🤝 参与贡献

欢迎贡献代码！请在提交 Pull Request 之前仔细阅读我们的 `CONTRIBUTING.md` 指南。

为保证代码库健康，请确保您的代码在提交前通过所有本地验证：

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## 📄 许可证

本项目采用 **GPL-3.0-or-later** 许可证发布。欢迎在许可证条款许可的范围内自由使用、修改和分发。

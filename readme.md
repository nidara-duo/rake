# <img src="assets/rake_logo.ico" width="42" height="42" valign="middle" alt="Rake Logo"> Rake (Scoop in Rust)

[![Version](https://img.shields.io/badge/version-0.1.0-blue?style=for-the-badge)](https://github.com/nidara-duo/rake/releases)
[![License](https://img.shields.io/badge/license-GPL--3.0-green?style=for-the-badge)](LICENSE)
[![Platform Windows](https://img.shields.io/badge/platform-windows-0078d7?style=for-the-badge&logo=windows)](https://www.microsoft.com/windows)
[![Language Rust](https://img.shields.io/badge/language-Rust-ea4aaa?style=for-the-badge&logo=rust)](https://www.rust-lang.org)

<div align="center">

[🇬🇧 English](readme.md) | [🇨🇳 简体中文](language/readme_cn.md)

</div>

**Rake** is a complete, clean-room implementation of the popular **Scoop** package manager for Windows, written entirely in Rust. 

Engineered to be a drop-in, blazing-fast, and lightweight replacement for the original PowerShell-based Scoop, Rake operates independently of the original codebase while remaining **100% compatible** with your existing environment, apps, manifests, and buckets.

---

## ⚡ Benchmarks (Rake vs. Scoop)

Thanks to Rust's native compilation and highly optimized I/O operations, Rake executes commands **tens to hundreds of times faster** than Scoop.

| Command | Scoop (Mean) | Rake (Mean) | Speedup |
| :--- | :--- | :--- | :--- |
| `list` (List installed apps) | ~582.90 ms | **~19.80 ms** | **~30x faster** |
| `status` (Check for updates) | ~4341.90 ms | **~28.10 ms** | **~154x faster** |
| `search rustup` (Search buckets) | ~2531.05 ms | **~282.55 ms** | **~9x faster** |
| `bucket list` (List repositories) | ~400.40 ms | **~196.00 ms** | **~2x faster** |
| `bucket add nonportable` | ~38063.15 ms | **~102.25 ms** | **~372x faster** |
| `bucket rm nonportable` | ~102.90 ms | **~12.75 ms** | **~8x faster** |

*Note: The* `checkup` *command optimization is currently work-in-progress (Scoop: ~75ms, Rake: ~908ms).*

---

## ✨ Features

- **Blazing Fast Performance** — No more waiting for cold PowerShell startup overhead.
- **Pure Autonomy** — Ships as a single standalone `rake.exe` binary. No heavy PowerShell session is spawned for daily tasks.
- **Flawless Scoop Compatibility** — Instantly inherits existing Scoop installation paths, configuration, local buckets, and downloaded cache.
- **Elegant Decoupling** — Package management logic stays inside the core engine, while self-updates and complex installations leverage a lightweight, non-intrusive bootstrap mechanism.
- **Secure by Default** — Strict SHA-256 validation for all remote assets and internal releases.

---

## ⚙️ Architecture

The project is split into a set of highly decoupled, modular crates to ensure code maintainability and testability:
- `rake-domain` — Pure domain types, manifest parsing, and strict validation rules (completely decoupled from I/O and network layers).
- `rake-core` — The heavy-lifter. Handles async execution, state management, package operations, and the internal Event Bus.
- `rake-cli` — Terminal frontend powered by `Clap` for argument parsing and `Indicatif` for beautiful interactive progress bars.
- `rake-hash` — Highly efficient file hashing backend.
- `rake-shim-bin` — Ultra-lightweight Windows executable templates for low-overhead application shimming.

---

## 🚀 Installation

### Method 1: Automated Bootstrap Script (Recommended)

You can install Rake via a single PowerShell command. The script automatically detects your CPU architecture (`x86_64`, `i686`, `aarch64`), pulls the latest release, verifies hashes, and updates your system `PATH`:

```powershell
powershell -ExecutionPolicy Bypass -Command "iwr -useb https://raw.githubusercontent.com/nidara-duo/rake/main/scripts/bootstrap.ps1 | iex"
```

Method 2: Manual Build from Source
If you prefer building it yourself, ensure you have the Rust toolchain installed:

Clone the repository:

```Bash
git clone https://github.com/nidara-duo/rake.git
cd rake
```

Compile the production release:

```Bash
cargo build --release
```

Your compiled binary will be available at `target/release/rake.exe`.

🔄 Self-Updating
Rake cleanly separates package management from binary orchestration. To safely update rake.exe to the latest stable release without locking files currently in use, simply run:

PowerShell
rake self update
This triggers the background bootstrap wrapper to fetch, verify, and cleanly swap the executable in place.

🛠️ Available Commands
Rake ships with a comprehensive set of CLI commands, fully mirroring the Scoop workflow:

```Plaintext
Commands:
  alias       Manage scoop aliases
  bucket      Manage buckets
  cache       Manage download cache
  cat         Show content of specified manifest
  checkup     Check for potential problems
  cleanup     Remove old versions of packages
  config      Get or set configuration values
  download    Download packages to cache
  export      Export installed apps, buckets and configs in JSON format
  hold        Hold package(s) to disable changes
  home        Browse the homepage of a package
  import      Import apps, buckets and configs from a Scoopfile in JSON format
  info        Show package(s) basic information
  install     Install an app
  list        List installed apps
  reset       Reset an app to resolve conflicts
  search      Search available packages
  self        Manage Rake itself (install, update, uninstall)
  shim        Manipulate Scoop shims
  status      Show status and check for new app versions
  unhold      Unhold package(s) to enable changes
  uninstall   Uninstall an app
  update      Update installed packages to latest versions
  which       Locate a shim/executable
```

🤝 Contributing
Contributions are welcome! Please read our `CONTRIBUTING.md` guide **before opening a Pull Request**.

To keep the codebase healthy, make sure your code passes all local validation pipelines before submitting:

```PowerShell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

📄 License
This project is licensed under the **GPL-3.0-or-later** license. Feel free to use, modify, and distribute it under the terms of the license.
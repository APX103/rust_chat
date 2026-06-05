# Rust stdio MCP Server Examples

三个**零 Node.js / 零 Python**的 stdio MCP server，全部用 Rust 写成，可静态链接到 musl。

| Server | 功能 | 外部依赖 |
|--------|------|---------|
| `mcp-time-rs` | 返回当前 UNIX 时间戳 | 纯 std |
| `mcp-filesystem-rs` | 读/写/列目录 | `serde_json` |
| `mcp-gpio-rs` | 通过 arduino-router 控制 UNO Q GPIO | `serde_json` + `rmp-serde` |
| `mcp-led-matrix-rs` | 控制 UNO Q 板载 8×13 LED 矩阵 | `serde_json` + `rmp-serde` |

## 编译

### macOS 本机（测试用）

```bash
cd mcp-time && cargo build --release
cd ../mcp-filesystem && cargo build --release
cd ../mcp-gpio && cargo build --release
```

### 交叉编译到 UNO Q（ARM64 + musl）

```bash
# 在 mini-agent 项目根目录已有 .cargo/config.toml 配置 linker
cd examples/mcp-gpio
rustup target add aarch64-unknown-linux-musl
cargo build --release --target aarch64-unknown-linux-musl

# 结果：target/aarch64-unknown-linux-musl/release/mcp-gpio-rs
# scp 到 UNO Q 即可
```

## 配置

把编译好的二进制放到 UNO Q（例如 `/usr/local/bin/`），然后在 `.mini-agent/config.toml` 里配：

```toml
[mcp_servers.time]
command = "/usr/local/bin/mcp-time-rs"
args = []
timeout = 5

[mcp_servers.filesystem]
command = "/usr/local/bin/mcp-filesystem-rs"
args = []
timeout = 10

[mcp_servers.gpio]
command = "/usr/local/bin/mcp-gpio-rs"
args = []
timeout = 5
```

## GPIO 使用前提

`mcp-gpio-rs` 需要：
1. UNO Q 的 STM32 MCU 已刷入 Bridge RPC sketch（见 `docs/gpio-uno-q-research.md`）
2. `arduino-router` 后台服务正在运行
3. `/var/run/arduino-router.sock` 存在

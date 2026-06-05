# Arduino UNO Q GPIO 控制调研

> **⚠️ 状态：未验证（UNVERIFIED）**
>
> 以下信息基于 Arduino 官方文档和公开资料整理，尚未在真实 UNO Q 硬件上实际测试验证。
> 具体引脚编号、RPC 协议格式、arduino-router 行为等以实际测试为准。

---

## 1. 架构概览

Arduino UNO Q 采用 **双处理器异构架构**：

| 组件 | 芯片 | 运行系统 | 能否直接控制 GPIO |
|------|------|---------|------------------|
| MPU（微处理器） | Qualcomm Dragonwing QRB2210 (Cortex-A53) | Debian Linux | ❌ **不能直接控制 GPIO** |
| MCU（微控制器） | STM32U585 (Cortex-M33) | Zephyr RTOS + Arduino sketch | ✅ **GPIO 全部在此侧** |

UNO Q 共有 **47 个数字 GPIO**，全部由 STM32 MCU 控制。Qualcomm MPU 侧没有任何物理 GPIO 引脚引出。

---

## 2. MPU ↔ MCU 通信机制

### 2.1 中间件：arduino-router

UNO Q 预装了一个名为 **`arduino-router`** 的 Linux 后台服务，负责 MPU 和 MCU 之间的数据路由：

- **物理连接**：MPU 的 `/dev/ttyHS1` ↔ MCU 的 `Serial1`（串口）
- **网络拓扑**：星型拓扑（Star Topology）
- **通信协议**：MessagePack RPC
- **Linux 侧接口**：Unix Domain Socket `/var/run/arduino-router.sock`

> ⚠️ **警告**：`/dev/ttyHS1` 和 `Serial1` 被 arduino-router 独占锁定，用户代码**不可**直接访问。

### 2.2 RPC 调用流程

```
Linux 进程（Python/Rust）
    ↓ Unix Socket (/var/run/arduino-router.sock)
arduino-router（Linux 后台服务）
    ↓ 串口 (/dev/ttyHS1 ↔ Serial1)
STM32 MCU（Zephyr RTOS）
    ↓ 调用本地函数
GPIO 硬件
```

---

## 3. 从 Linux 侧控制 GPIO 的步骤

### 3.1 第一步：在 MCU 上刷入 Sketch

**必须先**用 Arduino IDE 或 Arduino App Lab 把下面的 sketch 刷到 STM32 MCU：

```cpp
#include "Arduino_RouterBridge.h"

void setup() {
    Bridge.begin();
    Bridge.provide_safe("gpio_write", gpio_write);
    Bridge.provide_safe("gpio_read", gpio_read);
}

void loop() {}

void gpio_write(int pin, bool value) {
    pinMode(pin, OUTPUT);
    digitalWrite(pin, value ? HIGH : LOW);
}

bool gpio_read(int pin) {
    pinMode(pin, INPUT);
    return digitalRead(pin) == HIGH;
}
```

关键 API：
- `Bridge.begin()` — 初始化 RPC 通信
- `Bridge.provide_safe(name, fn)` — 把本地函数暴露为 RPC 服务，**在 main loop 上下文安全执行**
- `Bridge.provide(name, fn)` — 在后台 RPC 线程直接执行（不能调用 Arduino API，如 `digitalWrite`）

### 3.2 第二步：Linux 侧通过 Unix Socket 调用

#### Python 示例（官方文档提供）

```python
import socket
import msgpack

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect("/var/run/arduino-router.sock")

# 调用 MCU 上的 gpio_write(D13, True)
request = msgpack.packb([0, 1, "gpio_write", [13, True]])
sock.sendall(request)

response = msgpack.unpackb(sock.recv(1024))
print(response)  # 返回 RPC 结果
```

依赖安装：
```bash
sudo apt install python3-msgpack
```

#### Rust 思路

需要引入 `rmp-serde`（MessagePack）和 `serde` crate：

```rust
use std::os::unix::net::UnixStream;
use std::io::{Read, Write};

fn call_rpc(method: &str, params: Vec<rmpv::Value>) -> rmpv::Value {
    let mut sock = UnixStream::connect("/var/run/arduino-router.sock").unwrap();
    // 按 MessagePack-RPC 格式序列化请求
    // 格式：[type, msgid, method, params]
    let request = rmpv::Value::Array(vec![
        rmpv::Value::Integer(0.into()),           // type = request
        rmpv::Value::Integer(1.into()),           // msgid
        rmpv::Value::String(method.into()),       // method
        rmpv::Value::Array(params),               // params
    ]);
    let packed = rmp_serde::to_vec(&request).unwrap();
    sock.write_all(&packed).unwrap();
    
    let mut buf = [0u8; 1024];
    let n = sock.read(&mut buf).unwrap();
    rmp_serde::from_slice(&buf[..n]).unwrap()
}
```

> ⚠️ **未验证**：上述 Rust 代码中的 MessagePack-RPC 请求格式需要根据实际 `arduino-router` 实现调整。Arduino 官方文档只提供了 Python 示例，Rust 侧需要自行适配。

---

## 4. 让 mini-agent（LLM）调用 GPIO

有三种接入路径，按推荐程度排序：

### 路径 A：在 mini-agent 中注册内置 `gpio` 工具（推荐）

在 `main.rs` 的 `register_builtin_tools()` 函数中添加一个 `gpio` 工具：

```rust
registry.register_tool(
    ToolSchema {
        name: "gpio".to_string(),
        description: "Control GPIO pins on Arduino UNO Q via RPC".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["write", "read"] },
                "pin": { "type": "integer", "description": "Arduino pin number (e.g. D13 = 13)" },
                "value": { "type": "boolean", "description": "true = HIGH, false = LOW (only for write)" }
            },
            "required": ["action", "pin"]
        }),
    },
    Arc::new(move |_name: &str, args: &Value| {
        let action = args["action"].as_str().unwrap_or("");
        let pin = args["pin"].as_i64().unwrap_or(0) as u8;
        
        // TODO: 连接 /var/run/arduino-router.sock
        // TODO: 发送 MessagePack RPC 请求
        // TODO: 处理响应
        
        match action {
            "write" => {
                let value = args["value"].as_bool().unwrap_or(false);
                // call_rpc("gpio_write", vec![pin.into(), value.into()])
                Ok(format!("Set pin {} to {}", pin, if value { "HIGH" } else { "LOW" }))
            }
            "read" => {
                // call_rpc("gpio_read", vec![pin.into()])
                Ok(format!("Pin {} value: true", pin))
            }
            _ => Err(anyhow!("Unknown GPIO action: {}", action))
        }
    }),
    ToolSource::Builtin,
);
```

**优点**：
- 零额外进程，不占用 UNO Q 资源
- 和 mini-agent 同进程运行，延迟最低
- 不需要安装 Python 等依赖

**缺点**：
- 需要写 Rust MessagePack-RPC 客户端
- 和 arduino-router 协议耦合

### 路径 B：独立 stdio MCP 服务器（Python）

写一个 Python 脚本作为 MCP stdio 服务器：

```python
#!/usr/bin/env python3
# gpio_mcp_server.py

import sys, json, msgpack, socket

def call_rpc(method: str, params: list):
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect("/var/run/arduino-router.sock")
    request = msgpack.packb([0, 1, method, params])
    sock.sendall(request)
    response = msgpack.unpackb(sock.recv(1024))
    return response

def handle_tool_call(name: str, args: dict) -> str:
    if name == "gpio_write":
        call_rpc("gpio_write", [args["pin"], args["value"]])
        return f"Set pin {args['pin']} to {args['value']}"
    elif name == "gpio_read":
        result = call_rpc("gpio_read", [args["pin"]])
        return f"Pin {args['pin']} value: {result}"
    return "Unknown tool"

# MCP stdio 协议主循环
while True:
    line = sys.stdin.readline()
    if not line:
        break
    req = json.loads(line)
    # ... 解析 MCP 请求，调用 handle_tool_call，输出 JSON 响应 ...
```

在 `config.toml` 中注册：

```toml
[mcp_servers.gpio]
command = "python3"
args = ["/path/to/gpio_mcp_server.py"]
timeout = 5
```

**优点**：
- 和 mini-agent 解耦，可独立维护
- Python 写 MessagePack RPC 更简单

**缺点**：
- UNO Q 上需要安装 `python3-msgpack`
- 多一个 Python 进程，略占资源

### 路径 C：独立 HTTP MCP 服务器

写一个轻量 HTTP 服务（可用 Rust `axum` 或 Python `flask`），内部通过 Unix Socket 调用 arduino-router。

**优点**：
- HTTP 协议通用，调试方便（可用 curl 直接测试）
- 不需要配置 stdio，和主程序完全解耦

**缺点**：
- 需要额外维护一个常驻 HTTP 服务
- 占用一个端口
- 延迟略高于同进程方案

---

## 5. UNO Q 引脚映射

GPIO 全部由 STM32 MCU 控制，UNO 标准连接器（22 个引脚）映射如下：

| MCU 引脚 | Arduino 引脚 | 功能 |
|---------|-------------|------|
| PB7 | D0 / RX | GPIO / UART RX |
| PB6 | D1 / TX | GPIO / UART TX |
| PB3 | D2 | GPIO |
| PB0 | D3 | GPIO / OPAMP OUT |
| PA12 | D4 / FDCAN1_TX | GPIO / CAN Bus TX |
| PA11 | D5 / FDCAN1_RX | GPIO / CAN Bus RX |
| PB1 | D6 | GPIO |
| PB2 | D7 | GPIO |
| PB4 | D8 | GPIO |
| PB8 | D9 | GPIO |
| PB9 | D10 / SS | GPIO / SPI SS |
| PB15 | D11 / MOSI | GPIO / SPI MOSI |
| PB14 | D12 / MISO | GPIO / SPI MISO |
| PB13 | D13 / SCK | GPIO / SPI SCK |
| PA4 | D14 / DAC0 | GPIO / ADC / DAC |
| PA5 | D15 / DAC1 | GPIO / ADC / DAC |
| PA6 | D16 | GPIO / ADC / OPAMP IN+ |
| PA7 | D17 | GPIO / ADC / OPAMP IN- |
| PC1 | D18 / SDA2 | GPIO / ADC / I2C SDA |
| PC0 | D19 / SCL2 | GPIO / ADC / I2C SCL |
| PB11 | D20 / SDA | GPIO / I2C SDA |
| PB10 | D21 / SCL | GPIO / I2C SCL |

另有 25 个引脚通过 JMISC 连接器引出。

> ⚠️ **未验证**：以上引脚映射来自官方文档，实际使用时建议先用 `gpioinfo`（如果 Linux 侧能看到）或 sketch 中的 `pinMode()` 逐一测试确认。

---

## 6. 准备工作清单

| 步骤 | 操作 | 环境 |
|------|------|------|
| 1 | 安装 Arduino IDE 或 Arduino App Lab | PC/Mac |
| 2 | 安装 UNO Q Zephyr Core（Boards Manager） | PC/Mac |
| 3 | 刷入 GPIO RPC sketch 到 STM32 MCU | UNO Q（USB 连接） |
| 4 | 确认 `arduino-router` 服务运行 | UNO Q Linux 终端 |
| 5 | 确认 `/var/run/arduino-router.sock` 存在 | UNO Q Linux 终端 |
| 6 | 测试 Python RPC 调用能否控制 LED | UNO Q Linux 终端 |
| 7 | 集成到 mini-agent（选路径 A/B/C） | UNO Q |

---

## 7. 推荐实施顺序

1. **先验证基础通路**：用 Python 示例脚本直接调用 `gpio_write` 控制 LED，确认 arduino-router + RPC 工作正常。
2. **再接入 mini-agent**：建议走 **路径 A**（Rust 内置工具），性能最好，无额外依赖。
3. **最后让 LLM 使用**：提供清晰的 tool description，让 LLM 知道可用引脚和功能。

---

## 8. 参考资料

- [Arduino UNO Q User Manual](https://docs.arduino.cc/tutorials/uno-q/user-manual/)
- [Arduino UNO Q Product Page](https://www.arduino.cc/product-uno-q)
- [libgpiod Documentation](https://libgpiod.readthedocs.io/)
- [MessagePack-RPC Spec](https://github.com/msgpack-rpc/msgpack-rpc/blob/master/spec.md)

---

*文档生成时间：2026-06-05*  
*状态：未验证 — 需在实际 UNO Q 硬件上测试后更新*

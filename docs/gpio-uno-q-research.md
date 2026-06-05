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

## 9. Arduino App Lab 深度调研（新增）

### 9.1 App Lab 的真实架构

Arduino App Lab 宣传上是"一站式开发环境"，但它的实际运行机制和直觉不太一样：

| 你以为的 | 实际上的 |
|---------|---------|
| Python 脚本直接跑在 Debian Linux 上 | ❌ **Python 脚本跑在 Docker 容器里** |
| `arduino.app_utils` 是系统级 Python 包 | ❌ **只在 App Lab 的容器内可用** |
| App 是一个常驻后台服务 | ❌ **需要手动点击 Run 才会启动** |
| 可以和系统其他进程自由通信 | ❌ **容器隔离，需要显式挂载才能访问宿主机资源** |

**关键证据**（来自 Arduino 官方论坛）：

> "The Python script of the App runs in a **Docker** container, isolated from the global Linux environment. So changes you make in the primary operating system's environment have no effect on the environment in which the Python script is executed, and vice versa."
> — Arduino 官方员工 ptillisch, 2025-11

### 9.2 App Lab 能做什么？

**适合的场景：**

1. **快速验证 Bridge RPC 通信**
   - 在 App Lab 里同时写 Python 侧和 Sketch 侧
   - 点击 Run，App Lab 自动编译 sketch、刷入 MCU、启动 Python
   - 有内置 Serial Monitor，可以看 MCU 输出

2. **刷入 MCU sketch**
   - App Lab 集成了固件刷写功能，比 Arduino IDE 更方便
   - 自动处理 MPU ↔ MCU 的通信初始化

3. **原型开发**
   - 快速测试 "Python 做 AI 推理 → MCU 控制 GPIO" 的完整链路
   - 内置 Bricks（摄像头、Web UI、AI 模型等）可以快速搭原型

**App Lab 开发流程：**

```
PC/Mac 上的 App Lab 编辑器
    ↓ 编辑文件（实际保存在 UNO Q 的 /home/arduino/arduino_apps/）
UNO Q 上的 App
    ├── sketch.ino  → 编译后刷入 STM32 MCU
    ├── main.py     → 在 Docker 容器里运行
    └── app.yaml    → App 配置
```

### 9.3 App Lab 不能做什么？

**不适合作为 mini-agent 的 GPIO 后端，原因：**

| 问题 | 说明 |
|------|------|
| 生命周期耦合 | App Lab 的 App 需要手动启动，mini-agent 无法自动拉起 |
| 环境隔离 | Docker 容器和 mini-agent 不在同一个进程空间 |
| API 不可调用 | `Bridge.call()` 在容器里，mini-agent 在宿主机，无法直接调用 |
| 端口暴露复杂 | 如果要在容器里暴露 HTTP API，需要配置端口映射 |
| 资源开销 | 多跑一个 Docker 容器，对 2GB 版本 UNO Q 是负担 |

**结论：不要把 App Lab 当作 GPIO 服务的实现方式。它只是一个开发和验证工具。**

### 9.4 那 App Lab 在这个项目里的正确用法是什么？

**阶段一：用 App Lab 验证通信（一次性）**

1. 在 App Lab 里创建一个新 App
2. **Sketch 侧**（刷入 MCU）：
   ```cpp
   #include "Arduino_RouterBridge.h"

   void setup() {
       pinMode(LED_BUILTIN, OUTPUT);
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
3. **Python 侧**（测试调用）：
   ```python
   from arduino.app_utils import Bridge
   import time

   # 测试：让 LED 闪烁 3 次
   for i in range(3):
       Bridge.call("gpio_write", LED_BUILTIN, True)
       time.sleep(0.5)
       Bridge.call("gpio_write", LED_BUILTIN, False)
       time.sleep(0.5)
   ```
4. 点击 Run，确认 LED 能正常闪烁
5. ✅ **通信验证完成**

**阶段二：脱离 App Lab，写独立服务（实际部署）**

验证通后，把 sketch 固定刷入 MCU（用 Arduino IDE 或 App Lab 刷一次即可），然后在 Debian Linux 侧写一个**独立的常驻服务**：

```python
#!/usr/bin/env python3
# gpio_service.py — 不依赖 App Lab，直接用系统 Python 运行

import socket
import msgpack
import sys

SOCKET_PATH = "/var/run/arduino-router.sock"

def call_rpc(method, params):
    """通过 Unix Socket 调用 MCU 上的 RPC 函数"""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.connect(SOCKET_PATH)
        request = msgpack.packb([0, 1, method, params])
        sock.sendall(request)
        response_data = sock.recv(1024)
        response = msgpack.unpackb(response_data)
        # response format: [type=1, msgid, error, result]
        return response[3] if len(response) > 3 else None

def gpio_write(pin, value):
    call_rpc("gpio_write", [pin, value])

def gpio_read(pin):
    return call_rpc("gpio_read", [pin])

if __name__ == "__main__":
    # 命令行测试
    if len(sys.argv) >= 3 and sys.argv[1] == "write":
        gpio_write(int(sys.argv[2]), sys.argv[3] == "1")
    elif len(sys.argv) >= 2 and sys.argv[1] == "read":
        print(gpio_read(int(sys.argv[2])))
```

这个脚本可以直接在 SSH 里运行，不需要 App Lab：

```bash
ssh arduino@uno-q-ip
sudo apt install python3-msgpack
python3 gpio_service.py write 13 1   # 点亮 D13 LED
python3 gpio_service.py read 13      # 读取 D13 状态
```

### 9.5 最终推荐架构

```
┌─────────────────────────────────────────────────────────────┐
│  macOS 开发机                                                │
│  ├── 交叉编译 mini-agent (cargo zigbuild)                     │
│  └── VS Code 远程调试                                         │
└──────────────────────┬──────────────────────────────────────┘
                       │ scp
                       ▼
┌─────────────────────────────────────────────────────────────┐
│  Arduino UNO Q (Debian Linux)                                │
│  │                                                           │
│  ├── mini-agent (Rust 二进制)                                │
│  │   └── 内置 gpio 工具 ──► Unix Socket                     │
│  │                           │                               │
│  │   ┌───────────────────────┘                               │
│  │   ▼                                                       │
│  ├── arduino-router (后台服务)                               │
│  │   └── /var/run/arduino-router.sock                       │
│  │       │                                                   │
│  │   ┌───┘                                                   │
│  │   ▼                                                       │
│  └── /dev/ttyHS1 ◄─────串────► Serial1 (STM32 MCU)          │
│                                                           │
│  ┌─────────────────────────────────────────────────────────┐│
│  │  Arduino App Lab（仅用于开发验证，不跑在 production）     ││
│  │  └── 临时测试 Bridge 通信、刷 sketch                      ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

### 9.6 关键依赖清单（UNO Q 上需要装的）

| 组件 | 安装命令 | 用途 |
|------|---------|------|
| `arduino-router` | 预装 | MPU ↔ MCU 通信路由 |
| `python3-msgpack` | `sudo apt install python3-msgpack` | 独立 Python 脚本调用 RPC |
| `gpiod` | `sudo apt install gpiod` | 如果 MPU 侧有 GPIO（确认没有） |

> ⚠️ **注意**：不需要在 UNO Q 上安装 Arduino App Lab 本身（它预装在 eMMC 上），也不需要安装 `arduino.app_utils`（它在 App Lab 容器里）。

---

## 10. 调研结论

| 问题 | 结论 |
|------|------|
| 能不能用 Arduino App Lab 实现 GPIO 服务？ | **不能**。App Lab 是开发工具，不是后台服务框架。 |
| 那 App Lab 有什么用？ | **开发验证**：快速测试 Bridge RPC 通信、刷 sketch、看 Serial Monitor。 |
| 实际部署选哪条路？ | **路径 A（Rust 内置）** 或 **独立 Python 脚本**。都不依赖 App Lab。 |
| 需要自己在 UNO Q 上写什么？ | 一个常驻的轻量服务（Rust/Python），通过 Unix Socket 调用 arduino-router。 |
| 最简可行产品（MVP）是什么？ | 1. 用 App Lab 刷好 sketch → 2. 写个 Python 脚本测试 RPC → 3. 把调用逻辑集成到 mini-agent。 |

---

*文档生成时间：2026-06-05*  
*状态：未验证 — 需在实际 UNO Q 硬件上测试后更新*

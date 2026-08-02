# mbshark2

A cross-platform Modbus RTU serial capture tool with a GUI, written in Rust.

---

## 中文文档

### 简介

mbshark2 是一个带图形界面的 Modbus RTU 串口抓包工具，使用 Rust 编写，跨平台支持。它只抓取串口（RTU）数据，不抓取网络（TCP）数据。界面极简——除了设置面板，无需复杂交互，只需拖动查看抓包结果。

### 功能特性

- **只抓串口数据**：专注 Modbus RTU，不支持 TCP
- **全功能码支持**：0x01–0x2B 所有标准功能码，解析失败也显示原始字节和错误原因
- **请求/响应配对**：基于两层状态机（pending + timed_out 队列），超时时间可配置
- **不丢弃任何包**：超时后收到的响应生成 ORPHAN 条目，而非丢弃
- **高亮显示**：UI 上按角色着色（请求=蓝、响应=绿、孤儿=橙、错误=红）
- **纯文本导出**：导出为无颜色的文本文件
- **配置持久化**：串口设置保存为 TOML 配置文件，启动自动加载（退出自动存 + 面板「Save Config」手动存）
- **Slave 过滤**：可隐藏指定 slave 的条目（仍抓取不丢弃，仅显示与按钮导出跳过；自动导出保留完整输出）
- **单线程异步**：UI 主线程 + tokio 异步线程，不使用多线程

### 显示格式

UI 中加高亮，导出文件为纯文本（无颜色）。每个条目以 `[TAG]` 开头，后跟时间戳、计数器和十六进制原始字节：

```
[REQUEST ][22:03:40.775(      1)] 02 10 00 00 00 02 04 00 00 00 01 3d 2b
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
```

#### 场景示例

**1. 正常请求/响应**（同一事务，不分隔）：

```
[REQUEST ][22:03:40.775(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
[RESPONSE][22:03:40.802(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
```

**2. Modbus 异常响应**（Exception Response，同一事务不分隔）：

```
[REQUEST ][22:03:40.775(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
[RESPONSE][22:03:40.802(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: Exception
  Error: Illegal Data Address (code 2)
```

**3. 超时 Timeout**（请求发出后，超时时间内无响应且无后续请求；同一事务不分隔）：

```
[REQUEST ][22:03:40.775(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
[RESPONSE][22:03:41.275(      1)]
  Error: Timeout
```

**4. 无响应 No Response**（RTU 半双工：主站发出下一个请求意味着上一请求不可能再收到响应，立即判定；同一事务不分隔）：

```
[REQUEST ][22:03:40.775(      1)] xx xx xx xx xx xx
  Slave:   5(0x05)
  Read Holding Registers: from 0x0000, count 10
[RESPONSE][22:03:40.775(      1)]
  Error: No Response
---
[REQUEST ][22:03:40.775(      2)] xx xx xx xx xx xx
  Slave:   6(0x06)
  Read Holding Registers: from 0x0000, count 10
```

**5. 超时后收到响应**（ORPHAN 独立分隔，无法归属到正常 session）：

```
[REQUEST ][22:03:40.775(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
[RESPONSE][22:03:41.275(      1)]
  Error: Timeout
---
[ORPHAN  ][22:03:40.902(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Error: Response Timeout
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
```

#### 分隔规则

- **REQUEST + 直接 RESPONSE**（正常 / 异常 / Timeout / No Response）= 同一 session，**不分隔**
- **counter 变化**（不同事务）= **分隔**
- **ORPHAN / PARSE 条目** = **单独分隔**（无法归属到正常 session）

#### 无响应判定（Timeout vs No Response）

- **Timeout**：请求发出后，在配置的超时时间内未收到响应，且无后续请求（由后台 sweeper 触发）
- **No Response**：请求发出后，主站发出了下一个请求（无论给哪个 slave）。因为 RTU 是半双工单总线，主站同一时刻只能有一个未完成事务，新请求到来意味着旧请求不可能再收到响应，立即判定

### 架构

```
src/
├── protocol/          # Modbus 协议层
│   ├── crc.rs         #   CRC-16 校验
│   ├── frame.rs       #   RTU 帧验证 + 长度预测（frame_length_hint）
│   └── pdu.rs         #   PDU 解码（所有功能码）
├── capture/           # 串口采集层
│   ├── serial.rs      #   tokio-serial 异步串口封装
│   ├── framer.rs      #   帧边界检测（长度+CRC 主动切分，3.5 字符间隔兜底）
│   └── engine.rs      #   采集引擎（编排 framer + 配对）
├── session/           # 会话配对层
│   ├── model.rs       #   Entry 显示模型
│   └── pairing.rs     #   请求/响应配对状态机
├── render/            # 渲染层
│   ├── format.rs      #   共享文本格式（UI + 导出共用）
│   ├── ui_view.rs     #   egui 抓包列表渲染
│   └── settings.rs    #   串口设置面板
├── export.rs          # 纯文本导出
├── config.rs          # TOML 配置持久化
├── app.rs             # eframe 应用入口
└── main.rs            # 二进制入口
```

线程模型：UI 主线程（egui 即时模式）+ tokio 单线程异步运行时，通过 mpsc 通道通信。

### 构建与运行

#### 系统依赖（Linux）

```bash
sudo apt install libudev-dev pkg-config libxcb1 libxkbcommon-x11-0 libgtk-3-dev
```

#### 编译运行

```bash
cargo run --release
```

> 如需只编译纯逻辑层（不含 GUI），使用 `cargo build --no-default-features`。

### 测试

```bash
cargo test --release
```

包含 72 个单元测试 + 3 个 E2E 测试（基于 `socat` 虚拟串口对）。

E2E 测试需要安装 `socat`：

```bash
sudo apt install socat
```

### 脚本化支持（环境变量）

便于自动化测试，无需手动操作 GUI：

| 环境变量 | 作用 |
|---------|------|
| `MBSHARK_AUTOSTART_PORT=/tmp/mb_a` | 预填端口并首帧自动开始抓包 |
| `MBSHARK_AUTOEXPORT_PATH=/tmp/out.txt` | 每秒自动导出纯文本结果 |

配合 `socat` 虚拟串口和 `tools/replay_modbus.py` 回放脚本使用：

```bash
# 1. 创建虚拟串口对
socat pty,raw,echo=0,link=/tmp/mb_a pty,raw,echo=0,link=/tmp/mb_b &

# 2. 启动 mbshark2（自动抓包 + 自动导出）
MBSHARK_AUTOSTART_PORT=/tmp/mb_a MBSHARK_AUTOEXPORT_PATH=/tmp/capture.txt \
  cargo run --release &

# 3. 回放 Modbus 流量
python3 tools/replay_modbus.py /tmp/mb_b
```

### 配置持久化与 Slave 过滤

- **配置文件**：`$XDG_CONFIG_HOME/mbshark2/config.toml`（Linux，默认 `~/.config/mbshark2/config.toml`；macOS `~/Library/Application Support/mbshark2/`；Windows `%APPDATA%/mbshark2/`）。保存端口 / 波特率 / 帧格式 / 超时 / 自动滚动 / 隐藏 slave 列表。
- **保存时机**：退出应用时自动保存；设置面板点「💾 Save Config」立即保存。面板底部显示当前配置文件路径。
- **加载优先级**：启动时加载配置 → `MBSHARK_AUTOSTART_PORT` 环境变量仍覆盖端口（env > 配置 > 默认值）。配置文件缺失或损坏时静默回退默认值，不阻断启动。
- **Slave 过滤**：设置面板「Hide slaves」填入逗号分隔的 slave id（如 `2,3,5`），这些 slave 的条目从主视图与「💾 Export…」导出中隐藏。**仍被抓取、不丢弃**——仅渲染/导出时跳过；`MBSHARK_AUTOEXPORT_PATH` 自动导出保留全部 slave 以保证脚本输出完整。隐藏按整个 session 生效（请求 + 其响应/超时/孤儿）。

---

## English Documentation

### Overview

mbshark2 is a cross-platform Modbus RTU serial capture tool with a GUI, written in Rust. It captures only serial (RTU) traffic — network (TCP) is not supported. The UI is minimal: aside from the settings panel, no complex interaction is required — just scroll through the captured results.

### Features

- **Serial-only capture**: Focused on Modbus RTU; no TCP support
- **All function codes**: 0x01–0x2B standard function codes; parse failures display raw bytes and the error reason
- **Request/response pairing**: Two-layer state machine (pending + timed_out queues) with a configurable timeout
- **No packet loss**: Responses arriving after timeout become ORPHAN entries instead of being discarded
- **Syntax highlighting**: UI colors entries by role (request=blue, response=green, orphan=orange, error=red)
- **Plain-text export**: Exports to a colorless text file
- **Config persistence**: Serial settings are saved to a TOML config file and auto-loaded on launch (auto-save on exit + a "Save Config" button)
- **Slave filtering**: Hide entries for given slaves (still captured, never discarded — only skipped at display/export; auto-export stays complete)
- **Single-threaded async**: UI main thread + tokio async thread, no multi-threading

### Display Format

The UI applies highlighting; exported files are plain text (no color). Each entry starts with a `[TAG]`, followed by a timestamp, counter, and hex raw bytes:

```
[REQUEST ][22:03:40.775(      1)] 02 10 00 00 00 02 04 00 00 00 01 3d 2b
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
```

#### Scenario Examples

**1. Normal request/response** (same session, no separator):

```
[REQUEST ][22:03:40.775(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
[RESPONSE][22:03:40.802(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
```

**2. Modbus exception response** (same session, no separator):

```
[REQUEST ][22:03:40.775(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
[RESPONSE][22:03:40.802(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: Exception
  Error: Illegal Data Address (code 2)
```

**3. Timeout** (no response within the configured timeout and no follow-up request; same session, no separator):

```
[REQUEST ][22:03:40.775(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
[RESPONSE][22:03:41.275(      1)]
  Error: Timeout
```

**4. No Response** (RTU is half-duplex: the master issuing the next request means the previous one can no longer be answered — judged immediately; same session, no separator):

```
[REQUEST ][22:03:40.775(      1)] xx xx xx xx xx xx
  Slave:   5(0x05)
  Read Holding Registers: from 0x0000, count 10
[RESPONSE][22:03:40.775(      1)]
  Error: No Response
---
[REQUEST ][22:03:40.775(      2)] xx xx xx xx xx xx
  Slave:   6(0x06)
  Read Holding Registers: from 0x0000, count 10
```

**5. Late response after timeout** (ORPHAN separated — cannot belong to a normal session):

```
[REQUEST ][22:03:40.775(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
[RESPONSE][22:03:41.275(      1)]
  Error: Timeout
---
[ORPHAN  ][22:03:40.902(      1)] xx xx xx xx xx xx
  Slave:   2(0x02)
  Error: Response Timeout
  Write Holding Registers: from 0x0000, count 2
    0x0000: 0x0000(0)
    0x0001: 0x0001(1)
```

#### Separator Rules

- **REQUEST + direct RESPONSE** (normal / exception / Timeout / No Response) = same session, **no separator**
- **Counter change** (different transaction) = **separator**
- **ORPHAN / PARSE entries** = **separated** (cannot belong to a normal session)

#### No-Response Judgement (Timeout vs No Response)

- **Timeout**: After a request is sent, no response arrives within the configured timeout and no follow-up request occurs (triggered by the background sweeper)
- **No Response**: After a request is sent, the master issues the next request (to any slave). Since RTU is a half-duplex single bus with at most one outstanding transaction at a time, a new request means the previous one can no longer be answered — judged immediately

### Architecture

```
src/
├── protocol/          # Modbus protocol layer
│   ├── crc.rs         #   CRC-16 verification
│   ├── frame.rs       #   RTU frame validation + length prediction (frame_length_hint)
│   └── pdu.rs         #   PDU decoding (all function codes)
├── capture/           # Serial capture layer
│   ├── serial.rs      #   tokio-serial async wrapper
│   ├── framer.rs      #   Frame boundary detection (length+CRC split primary, 3.5-char gap fallback)
│   └── engine.rs      #   Capture engine (orchestrates framer + pairing)
├── session/           # Session pairing layer
│   ├── model.rs       #   Entry display model
│   └── pairing.rs     #   Request/response pairing state machine
├── render/            # Rendering layer
│   ├── format.rs      #   Shared text formatting (UI + export)
│   ├── ui_view.rs     #   egui capture list rendering
│   └── settings.rs    #   Serial settings panel
├── export.rs          # Plain-text export
├── config.rs          # TOML config persistence
├── app.rs             # eframe application entry
└── main.rs            # Binary entry point
```

Threading model: UI main thread (egui immediate mode) + tokio single-threaded async runtime, communicating via mpsc channels.

### Build & Run

#### System Dependencies (Linux)

```bash
sudo apt install libudev-dev pkg-config libxcb1 libxkbcommon-x11-0 libgtk-3-dev
```

#### Build and Run

```bash
cargo run --release
```

> To build only the pure logic layer (without GUI), use `cargo build --no-default-features`.

### Testing

```bash
cargo test --release
```

Includes 72 unit tests + 3 E2E tests (using `socat` virtual serial port pairs).

E2E tests require `socat`:

```bash
sudo apt install socat
```

### Scripting Support (Environment Variables)

Enables automated testing without manual GUI interaction:

| Env var | Purpose |
|---------|---------|
| `MBSHARK_AUTOSTART_PORT=/tmp/mb_a` | Pre-fills the port and auto-starts capture on the first frame |
| `MBSHARK_AUTOEXPORT_PATH=/tmp/out.txt` | Auto-exports plain-text results every second |

Use with `socat` virtual ports and the `tools/replay_modbus.py` replay script:

```bash
# 1. Create a virtual serial port pair
socat pty,raw,echo=0,link=/tmp/mb_a pty,raw,echo=0,link=/tmp/mb_b &

# 2. Launch mbshark2 (auto-start capture + auto-export)
MBSHARK_AUTOSTART_PORT=/tmp/mb_a MBSHARK_AUTOEXPORT_PATH=/tmp/capture.txt \
  cargo run --release &

# 3. Replay Modbus traffic
python3 tools/replay_modbus.py /tmp/mb_b
```

### Config Persistence & Slave Filtering

- **Config file**: `$XDG_CONFIG_HOME/mbshark2/config.toml` (Linux, default `~/.config/mbshark2/config.toml`; macOS `~/Library/Application Support/mbshark2/`; Windows `%APPDATA%/mbshark2/`). Stores port / baud / framing / timeout / auto-scroll / hidden slaves.
- **Save timing**: Auto-saved on exit; the "💾 Save Config" button saves immediately. The current config path is shown at the bottom of the settings panel.
- **Load priority**: Loaded on launch → `MBSHARK_AUTOSTART_PORT` still overrides the port (env > config > defaults). A missing or corrupt config file silently falls back to defaults without blocking startup.
- **Slave filtering**: Enter comma-separated slave ids in the "Hide slaves" field (e.g. `2,3,5`) to hide those slaves from the main view and the "💾 Export…" output. They are **still captured, never discarded** — only skipped at render/export; `MBSHARK_AUTOEXPORT_PATH` keeps all slaves so scripted output stays complete. Hiding applies per whole session (request + its response/timeout/orphan).

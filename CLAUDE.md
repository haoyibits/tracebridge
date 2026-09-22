# tracebridge：用 Rust 重写 trace32-bridge，做成全局 CLI

## 背景
我有一个 Python 工具，用来把 Lauterbach TRACE32 PowerView 接进 VS Code：烧录、加载符号、调试适配代理、RTT 终端、生成 VS Code 配置。
现在它依赖 Python 和虚拟环境，而且要把工具包复制进每个项目里。我想把它重写成 Rust 单文件静态二进制，装进系统后，在任意项目目录直接运行 `tracebridge <子命令>` 就能用。

## 参考实现（只读，以它为准，不要猜）
- Python 工具：/Users/haoyi/Documents/Code/Thesis/trace32-bridge（HEAD 3ec3684 "env fix"）
  - 入口 trace32_bridge/cli.py；模块有 config.py、powerview.py、target.py、remote.py、rtt.py、
    dap/protocol.py、dap/proxy.py、vscode/installer.py、vscode/jsonc.py、cmm/toolbar.cmm
  - vscode/tasks.json、vscode/launch.json 是模板，trace32.toml 是配置示例
  - tests/ 是行为规格，要移植成 Rust 测试
- Lauterbach RCL Python 库 v1.1.5（MIT 许可，纯 Python，基于 socket）：
  /Users/haoyi/Library/Python/3.14/lib/python/site-packages/lauterbach/trace32/rcl
  - TCP 协议在 _rc/hlinknet.py（CommunicationTcp、Link）和 _rc/_library.py（Library）
  - 高层 API 在 rcl.py 和 _rc/*.py（_memory.py、_address.py、_symbol.py、_practice.py、_functions.py 等）
  - 许可证原文：site-packages/lauterbach_trace32_rcl-1.1.5.dist-info/licenses/，"Copyright (c) 2020 Lauterbach GmbH"

## 目标
- 单个静态二进制，不依赖 Python 或任何运行时。（Linux 用 musl 完全静态；macOS 不能完全静态，只动态链接系统自带的 libSystem，这是 Apple 平台的常规做法。）
- 平台：macOS arm64/x86_64、Linux x86_64/aarch64（musl）。Windows 不是目标，但不要把它堵死，平台相关代码用 cfg(unix) 隔离。
- 用纯 Rust 实现 RCL 的 TCP 协议，**不链接 Lauterbach C API**，也不做 UDP。
- 功能和对外行为跟 Python 版一致，下面「新旧差异」列出的除外。

## 新旧差异（这是重写的核心动机）
Python 版假设工具包被复制进项目目录（config.py 的 TOOLKIT_DIR、trace32.toml 里的 root = ".."、install-vscode 最后执行 clean_toolkit）。新版是全局 CLI：
1. 配置发现：优先用 `--config <path>`；否则从 cwd 往上逐级找 `trace32.toml`，行为类似 git 找 .git。项目根目录 = toml 所在目录，所有相对路径以它为基准。
2. 运行时目录：Python 版是工具包下的 .run，新版改为 `<项目>/.tracebridge/`。用到 run_dir 或 env 目录的地方先对照 config.py 确认。
3. 内置资源（toolbar.cmm、VS Code 模板）用 include_str! 编进二进制。需要文件路径时，在运行时写到 run 目录。
4. 所有调用自身的地方都写 `std::env::current_exe()` 的绝对路径，并显式传 `--config <toml 绝对路径>`。涉及两处：
   - PowerView 工具栏按钮：原来是 `OS.Command "<python>" "<root>/t32.py" flash &`，改成 `OS.Command "<exe>" --config "<toml>" flash &`，保留末尾的 & 和路径安全检查
   - VS Code tasks：原来 command 是 __PYTHON_EXECUTABLE__ 加 t32.py 路径，改成 exe 的绝对路径（GUI 启动的 VS Code 不一定能拿到用户 shell 的 PATH）
5. 删除的部分：require_runtime_python、clean_toolkit、DEVELOPMENT_PATHS 及相关逻辑（连同 PRESERVED_GENERATED_DIRS、RCL_IMPORT、_can_import_rcl、_remove_path，以及 remote.py 里 "lauterbach-trace32-rcl is not installed" 的 ImportError 分支）。
6. 新增 `init` 子命令：在 cwd 生成带注释的 trace32.toml 模板（以旧项目的 trace32.toml 为蓝本，去掉 root = ".." 这类旧语义）。文件已存在时拒绝覆盖。
7. 保留 Python 版全部环境变量覆盖（完整列表见下文「环境变量」），以及它们的优先级和空值语义（_env_text、_env_override、_env_integer、_env_arguments 的区别）。

### 路径基准的具体变化（从 config.py 逐项核对）
| 项 | Python 版基准 | 新版基准 |
|---|---|---|
| 配置文件 | `--config`，否则 `TOOLKIT_DIR/trace32.toml` | `--config`，否则从 cwd 往上找 |
| project_dir | `project.root`（默认 ".."）或 PROJECT_ROOT，相对 toolkit | toml 所在目录（PROJECT_ROOT 见待确认 Q1） |
| project.elf / ELF | 相对 project_dir | 不变 |
| flash.script（非 `~~` 开头） | 相对 toolkit | 相对项目根 |
| trace32.sys / binary / config / debug_adapter 及对应环境变量 | 相对 **cwd**（`Path(...).resolve()`） | 相对项目根（按第 1 条"所有相对路径以它为基准"，见 Q1） |
| run_dir | `toolkit/.run` | `<项目>/.tracebridge/`（见 Q1） |
| toolbar.cmm | 包内文件 | 运行时写到 `run_dir/toolbar.cmm` |
| validate_common 里的 "toolkit path" 检查 | toolkit 路径 | 改为检查 toml 路径和 run_dir（它们都会拼进 TRACE32 命令） |

## 子命令
init、config、open、flash、load、adapter、rtt、vscode、rustrover。
- rtt 后面的参数原样转发给 rtt 自己的解析器（见 cli.py 的 main：在参数里找到第一个 "rtt"，其后的全部参数交给 rtt 解析器，rtt 的默认值来自已加载的配置）
- 错误统一输出为 `tracebridge: <msg>` 到 stderr，退出码 1；Ctrl-C 退出码 130
- `--version` 输出版本号和 git hash

## 架构
cargo workspace：
- `crates/t32rcl`：库。同步 API，基于 std::net，只实现本工具用到的 RCL 操作，错误类型区分连接失败、超时、TRACE32 返回的错误。
- `crates/tracebridge`：二进制。模块和 Python 文件一一对应（见「模块对照表」）。

### 依赖选型和理由
| 依赖 | 用在哪 | 理由 |
|---|---|---|
| clap（derive） | CLI | 子命令、`--config`、`--version`；rtt 子命令用 `trailing_var_arg + allow_hyphen_values` 收下全部剩余参数，再交给第二个 clap 解析器，等价于 cli.py 的转发 |
| toml | config | 只解析成 `toml::Table`，**不用 serde derive**：config.py 的 `_text/_integer/_number/_boolean/_string_list` 对每个键做类型检查，报错文案（"`{key} must be a string`" 等）必须一致，手写提取最直接 |
| serde_json（`preserve_order`、`arbitrary_precision`） | DAP、VS Code 文件 | Python 的 dict 保留插入顺序，json 往返不丢大整数；两个 feature 让重新编码后的键顺序和数字跟 Python 一致 |
| JSONC | vscode/jsonc | **自己写**（约 100 行，逐行移植 jsonc.py）。现成 crate（json5、jsonc-parser）接受的语法范围不同：jsonc.py 只去注释和 `}`/`]` 前的尾逗号，文件末尾的逗号必须报错，未闭合的块注释报 "unterminated block comment in JSONC"，读取时去掉 UTF-8 BOM。去完之后交给 serde_json |
| tokio（rt、net、io-util、process、signal、sync、time、macros） | 只在 adapter | 只在 adapter 子命令里建运行时，其他子命令保持同步 |
| thiserror | t32rcl 错误类型、BridgeError | 库里需要可匹配的错误枚举；应用层错误只是一条消息，所以不引入 anyhow |
| nix（term、process、signal） | rtt 终端、setsid | termios 用 nix 足够，crossterm 太重；PowerView 用 `pre_exec` 里调 `setsid()` 对应 `start_new_session=True` |
| chrono（仅 `clock`） | 备份文件名的本地时间戳 `%Y%m%d%H%M%S` | `time` crate 在多线程的 unix 上拿不到本地时区偏移 |
| shlex | `flash.args` 为字符串时、T32_FLASH_ARGS | **倾向自己移植** Python `shlex.split`（posix=True，comments=False）；阶段 2 用对照测试确认 shlex crate 对 `#` 等字符的处理是否一致，不一致就自己写 |
| tempfile（dev） | 测试 | |
| build.rs | `--version` 的 git hash | 调 `git rev-parse --short HEAD`，失败时用 "unknown"，不引入额外 crate |

需要照 Python 标准库语义自己移植的小函数（集中放在 `pycompat.rs`，各自有单测）：
- `os.path.expanduser` + `os.path.expandvars`（`$VAR`、`${VAR}`，未定义的变量原样保留；读的是真实进程环境，不是注入的 env）
- `Path.resolve()` 的非严格模式（路径不存在时也不报错：对已存在的最长前缀做规范化并解析符号链接，再拼上剩余部分）；`std::fs::canonicalize` 在路径不存在时会失败，不能直接用
- `int(text)`（环境变量整数：允许首尾空白、`+`/`-`、`_` 分隔）和 `int(text, 0)`（`control_block_address`、`--cb`：`0x`/`0o`/`0b` 前缀，十进制不允许前导 0）
- `shlex.split`（见上）

## 工程规则
- 协议字节和命令序列**必须以参考源码为准**，读不懂就问我，不要自己发明。
- 每个阶段结束时：cargo fmt、cargo clippy --all-targets -- -D warnings、cargo test 全部通过，然后 git commit（一个阶段一个或几个 commit）。
- 从 RCL 库移植的代码在文件头注明来源，并在 NOTICE 里保留 MIT 版权声明。
- 需要真实硬件或 PowerView 才能验证的部分，写成"手动验证清单"交给我，**不要声称已经验证过**。
- 每个阶段完成后**停下来**，向我汇报：做了什么、测试结果、手动验证清单、不确定的地方。等我确认后再进入下一阶段。

---

## RCL 操作清单

### 工具里用到的全部 RCL 调用（grep `debugger.`、`dbg.`、`rcl.` 的结果）
| Python 调用 | 位置 | 参数/备注 |
|---|---|---|
| `rcl.connect(node="localhost", port=str(rcl_port), protocol="TCP", packlen=1024, timeout=t)` | remote.py:17 `connect_debugger` | t 默认 5.0；`verify_rcl` 用 0.5 |
| `rcl.connect(node=args.node, port=str(args.port), protocol=args.protocol, packlen=1024, timeout=5.0)` | rtt.py:210 | protocol 可能是 UDP（见 Q6） |
| `with debugger:` 退出 | remote.py、target.py、powerview.py | → `disconnect()` |
| `debugger.cmd(str)` | target.py 多处、remote.py:33-34 | |
| `debugger.print(str)` | target.py:71、100，rtt.py:217 | |
| `debugger.cmm(str, timeout=)` | target.py:44（operation_timeout），powerview.py:134（10） | |
| `debugger.fnc.system_up()` | target.py:75 | |
| `debugger.fnc.state_run()` | target.py:98 | |
| `dbg.address.from_string("E:0x%X")` | rtt.py:45 | 纯本地 |
| `dbg.memory.read(addr, length=n)` | rtt.py:51、62、76、82、103 | n = 16 或环形缓冲区长度 |
| `dbg.memory.write(addr, bytes)` | rtt.py:119、121 | |
| `dbg.memory.write_uint32(addr, v)` | rtt.py:88、124、255 | |
| `debugger.symbol.query_by_name(name=...).address.value` | rtt.py:134 | name = `\\<program>\Global\<symbol>` |

### TCP 传输层（hlinknet.py `CommunicationTcp` + `Link`）
- **连接**：`AF_INET` + `SOCK_STREAM`（只用 IPv4，"localhost" 解析成 127.0.0.1），`settimeout(timeout)` 同时作用于 connect 和之后的**每一次** recv。
- **同步**：TCP 的 `sync()` 是空实现，直接返回 True。Library.sync 最多循环 5 次，第一次就 break。**TCP 没有任何握手报文**，UDP 的 magic/SYNC 报文不需要实现。
- **发送帧**：`u32 len | u32 type=0x0010 (RCL_REQ) | payload[len] | 补 0 到 8 字节对齐`（补齐字节数 = `align_eight(len + 8)`）。Python 用的是不带前缀的 `struct.pack("II")`，即本机字节序；四个目标平台都是小端，Rust 固定用 LE。
- **接收帧**：缓冲区累积后按 `u32 len | u32 type` 解析，负载后面补齐到 8 的倍数，补齐部分跳过。type 0x0011 → 响应队列，0x0012（NOTIFY）→ 通知队列（本工具不启用通知，丢掉），其他类型 → ConnectionError "TCP packet contains invalid messagetype"。recv 返回 0 字节 → "Connection closed by peer"。
- **包长**：TCP 下 `packlen` 参数不影响 socket（Link 固定用 `CommunicationTcp(0x4000, ...)`，每次 recv 最多读 0x4000 字节），但 `Library._maxpacketsize = link.packlen = 1024` 仍然有效，决定内存读写的分块大小（见下）。
- **断开**：`t32_exit` 只关闭 socket，不发任何报文。
- **超时**：recv 超时 → `socket.timeout` → Link.receive 转成 `ApiConnectionTimeoutError`。

### 请求/响应格式（`_library.py` `generic_api_call`）
- `msg_len = 2 + len(payload)`；有 `force_length` 时直接用它（内存读写用这个）。
- **短格式**（msg_len ≤ 0xFF 且没有强制 16 位长度）：`u8 msg_len | u8 rapi_cmd | u8 opt_arg | u8 msg_id | payload | (len(payload) % 2) 个 0`
- **长格式**：`u8 0 | u8 rapi_cmd | u8 opt_arg | u8 msg_id | u16 (msg_len + 2) | payload | 补齐`；`msg_len + 2 > 0xF000` → ApiProtocolTransmitError("message buffer too large")。
- **msg_id**：`Link._message_id` 从 0 开始，每发一个请求先 +1，报文里写 `_message_id % 255`，所以序列是 1, 2, …, 254, 0, 1, …（回绕时跳过 255）。重连（attach 超时重试）**不**重置计数。
- **响应匹配**（`CommunicationBase.receive`）：期望 id = `_message_id % 255`。依次取响应：空 → 跳过；`resp[0] == 0xFE`（KEEPALIVE）→ 跳过；`resp[1] < 期望` → 当作旧响应丢掉；`== 期望` → 返回；`> 期望` → ConnectionError "Messages out of sync, connection broken"。
- **错误**：`resp[0] != 0` 即错误码（只有 1 个字节）。`len(resp) > 10` 时，错误消息 = `resp[10 : 10 + u32le(resp[6:10])]` 按 UTF-8 解码（解码失败则无消息）。`raise_error` 查 `error_code_exception_mapping`：没有消息就用表里的默认文案；表里没有的码 → `ApiError(code, msg)`。对本工具重要的码：90 = T32_ERR_FN1（被 cmd/fnc/memory 各自改成 CommandError / FunctionError / MemoryAccessError）。
- **成功**：返回 `resp[2:]`。

### 各操作最终发出的底层消息
| 操作 | RCL 调用路径 | 底层消息 |
|---|---|---|
| **connect** | `Debugger.__init__` → `Library.t32_init`（TCP connect + 空 sync）→ `connect()` 里 `t32_attach(1)` → `check_powerview_version()` | ① TCP 连接；② ATTACH：cmd 0x71、opt 0x01、无负载；attach 抛 `ApiConnectionTimeoutError` 时：关闭 → 重连 → 再 attach 一次；③ fnc `SOFTWARE.BUILD()`；④ fnc `SOFTWARE.BUILD.BASE()`；base < 125398 或 build < 126615 → ApiVersionError "Minimum required software version: …"；⑤ fnc `VERSION.PYRCL(1.1.5)`，结果忽略（见 Q4） |
| **disconnect** | `__exit__` → `t32_exit` | 只关 socket |
| **cmd(s)** | `CommandService` → `Debugger._cmd` → `t32_executecommand(s.encode(), 4096)` | cmd 0x72（EXECUTE_PRACTICE）、opt 0x04，负载 = `u32le 4096 | s | 00`；FN1 → `CommandError(str(e), "command: ", cmd)` |
| **print(s)** | `cmd('ECHO "{s}"')` | 同 cmd |
| **fnc(expr)** | `FunctionService.__call__` → `_fnc` → `t32_executefunction` | cmd 0x72、opt 0x05，负载 = `u32le 4096 | expr | 00`；结果 `r = resp[2:]`：`type = u32le(r[0:4])`、`size = u32le(r[4:8])`、`value = r[8:8+size]` UTF-8；FN1 → FunctionError。按 `_decode_eval_result` 解码：0x0001 bool（"TRUE()"/"FALSE()"，其他值报 FunctionError），0x0002 二进制 `int(v[2:], 2)`，0x0004 十六进制 `int(v, 16)`，0x0008 十进制 `int(v[:-1])`（去掉末尾的点），0x0010 浮点，0x0020/0x0040/0x0080/0x0100/0x0200/0x4000 字符串，0x0400 时间，0x8000 空，0x0000 错误字符串 |
| **fnc.system_up()** | fnc `SYStem.Up()` | 同 fnc，期望 bool |
| **fnc.state_run()** | fnc `STATE.RUN()` | 同 fnc，期望 bool |
| **cmm(script, timeout)** | `Debugger.cmm` | ① fnc `PRACTICE.SD()` 记下深度 pre；② cmd `DO {script}`（CommandError → PracticeError）；③ timeout 为 None 或 > 0 时循环：fnc `PRACTICE.SD()`；小于 pre → PracticeError("Practice stack depth error")；等于 → 完成；超过 timeout → 内置 `TimeoutError()`；每轮 sleep 10 ms。timeout=0 不轮询 |
| **address.from_string("E:0x…")** | `Address.from_string` | **不发报文**。正则 `^(?:(access).+:)?(?:(machine).+:::)?(?:(space).+::)?(value 十进制或 0x 十六进制)$` → `Address{access="E", value}` |
| 地址序列化（读写/符号共用） | `Address.serialize(offset, width)` | `u16le 3 (A64) | u64le value+offset | "AC" u16le L access 补 0 到 L（L = len+1 向上取偶）| ["WI" u16le width] | "XX"`。"E" → `41 43 02 00 45 00` |
| **memory.read(addr, n)** | `MemoryService.read` → `t32_readmemoryobj(width=None)` | 分块大小 = `down_align(1024 − 10, 8)` = 1008；每块：cmd 0x74（DEVICE_SPECIFIC）、opt 0x35（MEMORY_OBJ_READ），负载 = `u16le chunk | 地址(offset = 已读字节数)`，`force_length = 地址长 + 6`；数据 = `resp[2:][:chunk]`。FN1 → MemoryAccessError("wrong parameters")；其他 InternalError → MemoryReadAccessError |
| **memory.write(addr, data)** | `t32_writememoryobj(width=None)` | 分块大小 = 1024；每块：cmd 0x74、opt 0x36（MEMORY_OBJ_WRITE），负载 = `u16le chunk | 地址(offset) | data 块`，`force_length = 地址长 + 6`（所以即使数据有 1024 字节，也按短格式发，长度字节只算地址部分，照抄）。其他 InternalError → MemoryWriteAccessError |
| **memory.write_uint32(addr, v)** | `struct.pack("<I")` → `write(width=4)` | 同 write，地址里多一个 `"WI" 04 00` |
| **symbol.query_by_name(name)** | `SymbolService._symbol_query` → `Symbol.query` → `t32_querysymbolobj` | cmd 0x74、opt 0x68（SYMBOL_QUERYOBJ）；流 = `"NM" u16le L name 补 0 到 L（L = (len+2) & ~1）| "XX"`；负载 = `u16le (流长 + 6) | 流`（源码注释说 +6 是绕 TRACE32 的 bug）；响应 `resp[2:]` 按 AD/NM/PT/SZ(u64)/NE/XX 解析；AD 里：`u16 type`（2 → u32 值，3 → u64 值），之后是 AC/WI/CO/SI/IM/AT/MU/TU/XX |

### 用 Python 库本身生成的参考字节（Link 打桩，不连网络；阶段 1 的单测直接用）
```
attach(1), id=1              04 00 00 00 10 00 00 00 02 71 01 01 00 00 00 00
cmd "Go", id=2               0c 00 00 00 10 00 00 00 09 72 04 02 00 10 00 00 47 6f 00 00 00 00 00 00
fnc "SYStem.Up()", id=3      14 00 00 00 10 00 00 00 12 72 05 03 00 10 00 00 53 59 53 74 65 6d 2e 55 70 28 29 00 00 00 00 00
read E:0x20000000 n=16, id=4 18 00 00 00 10 00 00 00 18 74 35 04 10 00 03 00 00 00 00 20 00 00 00 00 41 43 02 00 45 00 58 58
write E:0x20000000 "AB", id=5 1a 00 00 00 10 00 00 00 18 74 36 05 02 00 03 00 00 00 00 20 00 00 00 00 41 43 02 00 45 00 58 58 41 42 00 00 00 00 00 00
write_uint32 … 7, id=6       20 00 00 00 10 00 00 00 1c 74 36 06 04 00 03 00 00 00 00 20 00 00 00 00 41 43 02 00 45 00 57 49 04 00 58 58 07 00 00 00
symbol "\\app\Global\_SEGGER_RTT", id=7
                             26 00 00 00 10 00 00 00 24 74 68 07 26 00 4e 4d 1a 00 5c 5c 61 70 70 5c 47 6c 6f 62 61 6c 5c 5f 53 45 47 47 45 52 5f 52 54 54 00 00 58 58 00 00
id 回绕：… fe → 00 → 01
```
（生成脚本在会话草稿区，阶段 1 会把它放进 `crates/t32rcl/tools/`，用来重新生成这些字节。）

### t32rcl 错误类型设计
- `Connect`：TCP 连不上、对端关闭连接
- `Timeout`：connect 或 recv 超时（对应 ApiConnectionTimeoutError）；`cmm` 超过期限单独用一个变体（对应 Python 内置的 TimeoutError，target.py 会把它变成 "TRACE32 operation exceeded {n}s"）
- `Trace32 { code, kind, message }`：TRACE32 返回的错误码；kind 区分 Command / Function / Memory / Practice / Version / 其他
- `Protocol`：帧格式错误、非法消息类型、msg_id 错乱

---

## 模块对照表

### crates/t32rcl
| Rust 模块 | 对应 Python（RCL 库） | 测试 |
|---|---|---|
| `link.rs` | `_rc/hlinknet.py` CommunicationTcp、Link（分帧、msg_id、响应匹配） | 单测：帧编解码、补齐、粘包/半包、msg_id 回绕、KEEPALIVE、乱序 |
| `api.rs` | `_rc/_library.py` generic_api_call、t32_attach、t32_executecommand、t32_executefunction、t32_readmemoryobj、t32_writememoryobj、t32_querysymbolobj、raise_error、错误码表 | 单测：上面的参考字节；错误响应解析 |
| `address.rs` | `_rc/_address.py` | 单测：from_string、serialize、deserialize |
| `symbol.rs` | `_rc/_symbol.py` | 单测：serialize、deserialize |
| `eval.rs` | `rcl.py` `_decode_eval_result` | 单测：每种结果类型 |
| `error.rs` | `_rc/_error.py`、`_memory_exceptions.py`、`_library.py` 错误表 | |
| `lib.rs`（`Debugger`：connect、cmd、print、fnc、system_up、state_run、cmm、memory_read、memory_write、memory_write_u32、symbol_address、disconnect） | `rcl.py` Debugger、`_command.py`、`_functions.py`（子集）、`_memory.py`（子集） | `tests/fake_server.rs`：假 RCL 服务端回放字节，覆盖每个公开操作和错误路径；以后换成真实抓包 |
| `examples/capture_proxy.rs` | 新增 | TCP 转发器，按方向把流量存成 fixture |
| `examples/smoke.rs` | 新增 | 连 localhost:20000，执行 cmd、读 system_up、读内存（手动验证） |

### crates/tracebridge
| Rust 模块 | 对应 Python | 对应测试 |
|---|---|---|
| `main.rs` | `t32.py`、`cli.py`（main、_parser、_run、_print_config、info） | 新增 `tests/cli.rs`：rtt 参数转发、错误格式、退出码、`--version` |
| `errors.rs` | `errors.py` | — |
| `config.rs` | `config.py` | `test_config.py` 全部 5 条（toolkit_dir 改成 toml 目录）+ 新增：向上查找、`--config` 优先级、每个环境变量的空值语义 |
| `pycompat.rs` | Python 标准库语义（expanduser/expandvars、resolve、int、shlex.split） | 新增对照单测 |
| `t32config.rs` | 新增：读取 config.t32 的 RCL=/PORT= | 单测 |
| `ui.rs` | cli.py 的 `info`、current_exe | — |
| `init.rs` + `assets/trace32.toml` | 新增（蓝本是旧 trace32.toml） | 新增：生成内容能被 config 解析；已存在时拒绝覆盖 |
| `powerview.rs` + `assets/toolbar.cmm` | `powerview.py`、`cmm/toolbar.cmm` | `test_powerview.py` 4 条（toolbar 命令字符串改为 `"<run>/toolbar.cmm" "<exe>" "<toml>"`） |
| `remote.rs` | `remote.py` | `test_remote.py` |
| `target.rs` | `target.py` | `test_target.py` 4 条 + 新增：flash/load 完整命令序列断言（记录命令的测试替身） |
| `rtt.rs` | `rtt.py` | Python 没有 RTT 测试；新增环形缓冲区纯函数单测（回绕、满、空、未初始化、非法描述符） |
| `dap/protocol.rs` | `dap/protocol.py` | `test_dap_protocol.py` 4 条 + 边界补充 |
| `dap/proxy.rs` | `dap/proxy.py` | `test_dap_proxy.py` 2 条 + 新增：restart 成功/失败、后端连不上、端口被占用 |
| `vscode/installer.rs` + `assets/tasks.json`、`assets/launch.json` | `vscode/installer.py`、`vscode/*.json` | `test_vscode_installer.py` 的 merge、replace_tokens、install 3 条；**删掉** require_runtime_python 和 clean_toolkit 两条 |
| `vscode/jsonc.rs` | `vscode/jsonc.py` | `test_jsonc.py` 3 条 |
| `rustrover.rs` + `assets/rustrover.run.xml` | 新增（LSP4IJ DAPConfiguration） | 新增：生成的 XML 内容、已存在时备份后覆盖 |

## 环境变量（config.py 完整列表）
| 变量 | 语义 | 目标字段 |
|---|---|---|
| PROJECT_ROOT | `_env_text`（没设或为空 → 回退） | project_dir（见 Q1） |
| ELF | `_env_text` | elf |
| T32_SYS，其次 T32SYS | `_env_text` 嵌套：T32_SYS 非空优先，其次 T32SYS 非空，再其次 toml `sys`，都为空则 `~/t32` | t32_sys |
| T32_HOST | `_env_text`；最终为空时用 `_host_default()`（Darwin → macosx64，Linux → linux64，其他报错） | t32_host |
| T32_EXE | `_env_text`，默认 `t32marm-qt` | 可执行文件名 |
| T32_BIN / T32_CONFIG / T32_DEBUG_ADAPTER | `_env_text`；为空则由 sys/host 推导 | t32_binary / t32_config / debug_adapter |
| PROGRAM_NAME | `_env_override`（只要设了，**即使为空**也覆盖） | program |
| T32_CPU / T32_CORES / T32_MEMACCESS / T32_JTAG_CLOCK / T32_DUALPORT | `_env_override` | target.* |
| T32_FLASH_SCRIPT | `_env_override` | flash_script |
| T32_FLASH_ARGS | `_env_arguments`：只要设了就 `shlex.split`（空串 → 空列表）；解析失败报 "cannot parse T32_FLASH_ARGS: …" | flash_args |
| T32_RCL_PORT / T32_DAP_PORT / T32_DAP_BACKEND_PORT / T32_DAP_BACKEND_TIMEOUT / T32_TIMEOUT | `_env_integer`：空 → 回退；非整数报 "{name} environment override must be an integer" | 端口、超时 |
| RTT_SYMBOL | `_env_text` | rtt_symbol |
| T32_DAP_DEBUG | 运行时读取，等于 "1" 时给 t32debugadapter 加 `--log_level debug` | proxy |
| （路径里的 `$VAR`） | expandvars 读真实进程环境 | 所有 `_expand_path` |

不能用环境变量覆盖的：`rtos.*`、`rtt.control_block_address`、`rtt.poll_interval`。

---

## 已定的实现决策
1. **DAP 消息重新编码**：跟 Python 一样先解码再编码（`separators=(",",":")`、`ensure_ascii=False`），不做原样转发；serde_json 开 `preserve_order` + `arbitrary_precision` 以保持键顺序和数字。代理自己生成的响应里 `"message": null`、`"body": null` 要**显式输出**，不能省略。
2. **DapDecoder 的非 ASCII 头部**：Python 抛出的是 UnicodeDecodeError，不属于 handle_client 捕获的异常（会变成 asyncio 未处理异常）；Rust 统一当作协议错误结束会话。
3. **VS Code 输出格式**：`json.dumps(indent=4, ensure_ascii=False) + "\n"`，用 serde_json 的 4 空格 PrettyFormatter 实现；备份照 `shutil.copy2` 保留权限和修改时间。
4. **locals 过滤的原因**（阶段 4 写进注释）：来自 bd20db4 "add vscode debug" 的 README——某些 t32debugadapter 版本在一些 FreeRTOS 中断栈帧上读取 Locals 会报 `Invalid letter code` 然后**退出**。代理对 locals 作用域的 `variables` 请求直接回空列表，保证适配器不崩；Watch、寄存器、调用栈、断点和单步照常转发。
5. **clap 参数错误**保持 argparse 的行为：打印用法，退出码 2（不走 `tracebridge: <msg>` / 1 的路径）。
6. **PowerView 工具栏**只在本工具新启动 PowerView 时安装，复用已有实例时不装（与 Python 一致）。
7. **info 输出**保留 ANSI 颜色前缀，不判断是否 TTY（与 Python 一致）；前缀文字见「最终决定」Q8。

## 最终决定（2026-09-22：Q1–Q10 由子 agent 决定，用户追加了"不再需要 VS Code tasks、希望 RustRover 也能调试"）
- **Q1 路径基准**：项目根 = toml 所在目录。PROJECT_ROOT 保留，相对 toml 目录解析，只影响 ELF 的基准和 PowerView 的 cwd。toml 里出现 `project.root` 时报错：`project.root is no longer supported; the project root is the directory containing trace32.toml (<dir>); remove it`。run_dir 固定为 `<toml 目录>/.tracebridge/`，第一次创建时在里面写 `.gitignore`（内容 `*`）。flash.script（非 `~~`）以及 trace32.sys/binary/config/debug_adapter 的相对路径都相对 toml 目录。
- **Q2 读超时**：t32rcl 分开设置 connect 超时和 recv 超时。flash/load 连接用 5 秒，连上后 recv 超时改为 operation_timeout（原因：`FLASH.ReProgram OFF` 才真正写 Flash，镜像大时 5 秒不够）。其他场景照旧：recv 超时 = connect 超时。
- **Q3 分帧**：只修半包缓存（不足 8 字节的帧头、没收全的补齐字节都留在缓冲区）。msg_id 回绕、KEEPALIVE、丢弃旧响应照抄。
- **Q4 版本检查**：三条都照发（`SOFTWARE.BUILD()`、`SOFTWARE.BUILD.BASE()`、`VERSION.PYRCL(1.1.5)`），门槛沿用 125398/126615。本机装的是 R.2026.02（base 187884，build 190766）。
- **Q5 错误文案**：CommandError 显示为 `<msg> (command: <cmd>)`，其余文案不变。
- **Q6 RTT**：删掉 `--protocol`。保留 `--node --port --program --symbol --cb --poll --replay --output-only`。
- **Q7 RTT Ctrl-C**：退出码 0，stderr 打印 `TRACE32 RTT terminal stopped`，终端设置一定恢复。其他命令收到 Ctrl-C 仍按 130 退出。
- **Q8 名字**：统一叫 tracebridge。信息前缀 `[tracebridge]`（青色）；代理日志前缀 `[tracebridge]`，就绪那一行是 `[tracebridge] adapter listening on 127.0.0.1:<dap_port> (backend <backend_port>)`；PowerView 里 ECHO `tracebridge: flashed <elf>` 和 `tracebridge: symbols loaded for <program>`；toolbar.cmm 的 PRINT 用 `tracebridge: …`；rtt 找不到控制块时提示 `Run 'tracebridge load' first, or pass --cb 0x<address>.`
- **Q9 许可证**：MIT。LICENSE 写用户的版权；NOTICE 和 t32rcl 文件头保留 "Copyright (c) 2020 Lauterbach GmbH" 的 MIT 声明。
- **Q10 IDE 集成**（取代原来的 install-vscode；Flash/Load/RTT 等 task **全部去掉**，这些功能直接用 CLI）：
  - `tracebridge vscode`：合并写入 `.vscode/launch.json`（一条 `TRACE32: Attach`，`type: node`、`request: attach`、`debugServer`/`trace32Port` 是数字、`preLaunchTask: "tracebridge: adapter"`），以及 `.vscode/tasks.json` 里**唯一一个**隐藏的后台任务 `tracebridge: adapter`（command 是 exe 的绝对路径，args `["--config", "<toml 绝对路径>", "adapter"]`，beginsPattern `^\[tracebridge\] starting debug adapter`，endsPattern `^\[tracebridge\] adapter listening on 127\.0\.0\.1:<port>`）。合并规则照 merge_document，另外删掉旧版的 `T32: Flash`、`T32: Load ELF`、`T32: RTT Viewer`、`T32: Start Debug Adapter`、`T32: Flash + Debug`、`T32: Load + Debug`。写入前备份，原子写入。
  - `tracebridge rustrover`：写 `.run/TRACE32 Attach.run.xml`（IDE 会自动识别的共享运行配置），类型是 LSP4IJ 插件的 `DAPConfiguration`（RustRover 需要先装 "LSP4IJ" 插件）。配置里 command = `<exe> --config <toml> adapter`，debugMode = LAUNCH，debugServerWaitStrategy = TRACE，debugServerReadyPattern = `adapter listening on ${address}:${port}`，launchConfiguration = `{"type":"node","request":"attach","trace32Port":<rcl_port>}`，文件映射 `*.c;*.h;*.cpp;*.hpp;*.cc;*.s;*.S;*.rs`（没有映射的文件打不了断点）。
  - **原因**：LSP4IJ 的 LAUNCH 模式会自己启动命令，等日志匹配到就绪行后再连接，但它发出的是 `launch` 请求；ATTACH 模式则假定服务端已经在运行。因此代理新增一条改写规则（Python 版没有）：客户端发来 `launch`，且 `arguments.request == "attach"` 时，转发给后端的改成 `attach`，后端回的响应里 `command` 再改回 `launch`。VS Code 路径不受影响。
  - adapter 启动前先检查 RCL 端口，端口不通就报错 `no PowerView on RCL port <port>; run 'tracebridge open', 'flash' or 'load' first`。dap_port 已被占用时，照 Python 打印提示后以 0 退出。
- **其他**：
  - init 模板：去掉 root；`program` 默认用当前目录名，`elf` 默认 `build/<名字>.elf`；保留 SR6P6 target 参数和 FreeRTOS 配置；trace32.* 留空表示自动推导；生成后打印下一步该做什么。
  - config 输出：在原来的 ok/MISSING 列表上，加上 toml、run_dir、exe 路径和端口，以及 flash.script 是否存在；config.t32 里没有 `RCL=NETTCP`，或 `PORT=` 跟 rcl_port 对不上时打印 WARN。
  - 启动 PowerView：config.t32 里**没有** `RCL=` 行时，追加参数 `--t32-api-rcl=TCP:<rcl_port>`；有就不加，避免冲突。
  - 工具栏只保留 Flash / Load ELF 两个按钮。
- **工作方式**：用户 2026-09-22 指示"决定好后直接开始项目，最后告诉我使用方法"。因此各阶段连续推进，每阶段照样执行 fmt、clippy、test 并提交，全部完成后统一汇报，附使用方法和手动验证清单。

---

## 实施阶段

### 进度
| 阶段 | 状态 | 提交 |
|---|---|---|
| 0 规划 | ✅ | 2bb9b7a、54a24cc |
| 1 t32rcl | ✅ 与 Python 库生成的会话逐字节一致（tests/replay.rs） | 09fa2ef |
| 2 配置和 CLI | ✅ | 8970206 |
| 3 open/flash/load | ✅ | 2f843c5 |
| 4 adapter | ✅ | 313c46d |
| 5 rtt | ✅ 含 pty 下的终端模式测试 | 4513680 |
| 6 vscode/rustrover | ✅ | 67bc952 |
| 7 发布 | ✅ 手写 workflow（理由见下） | 见 git log |

**发布方案：手写 GitHub Actions，不用 cargo-dist。** 只有 4 个目标，产物就是 tar.gz 和 sha256，安装位置要求 `~/.local/bin`。cargo-dist 会生成它自己的安装器（默认装到 `~/.cargo/bin`），还要引入 dist 配置和每次重新生成的 workflow，收益小于维护成本。Linux 两个 musl 目标都在对应架构的原生 runner 上构建（ubuntu-24.04 / ubuntu-24.04-arm + musl-tools），不需要 cross；macOS 两个目标都在 macos-14 上构建。

### 手动验证清单（需要真实 PowerView/硬件，均未验证）
1. **t32rcl 对照**：PowerView 运行时，分别执行 `cargo run -p t32rcl --example smoke -- --address E:0x<RAM> --length 32` 和 `python3 crates/t32rcl/tools/smoke.py --address E:0x<RAM> --length 32`，两边输出应完全一致。
2. **真实抓包 fixture**：先运行 `cargo run -p t32rcl --example capture_proxy -- --out crates/t32rcl/tests/fixtures/powerview`，再运行 `python3 crates/t32rcl/tools/capture_session.py --port 20001 --session crates/t32rcl/tests/fixtures/powerview/session.txt --address E:0x<RAM> --symbol '\\<program>\Global\_SEGGER_RTT'`，最后 `cargo test -p t32rcl --test replay`。
3. **open 冷启动**：没有 PowerView 时执行 `tracebridge open`。PowerView 应能启动，`.tracebridge/powerview.log` 有内容，工具栏多出 Flash/Load ELF 两个按钮。
4. **open 复用**：PowerView 已在运行时执行 `tracebridge open`，应提示 reusing，且不装工具栏。
5. **flash / load**：在真实板子上执行，AREA 窗口应出现 `tracebridge: flashed …` 和 `tracebridge: symbols loaded for …`。大镜像烧录（`FLASH.ReProgram OFF` 很久）时不应超时。
6. **工具栏按钮**：分别点 Flash 和 Load ELF，项目路径带空格的情况也要试。ENTRY 的引号处理沿用 Python 版的写法，没有在真机上验证过。
7. **`--t32-api-rcl=TCP:<port>`**：从 config.t32 删掉 RCL 段后执行 `tracebridge open`，确认 PowerView 接受这个参数、端口可用；有 RCL 段时不应加这个参数。
8. **VS Code**：执行 `tracebridge vscode` 后按 F5，检查断点、单步、Restart、Locals 为空、停止调试后代理退出。
9. **RustRover**：装好 LSP4IJ 后执行 `tracebridge rustrover`，确认运行配置出现，Debug 能启动代理并连上，`.c` 文件里能打断点。**serverMappings 的 XML 格式是按 IntelliJ 序列化规则推断的**，如果不生效，就在 Mappings 页手动添加文件名模式。
10. **RTT**：在真实目标上检查输出、键盘输入，Ctrl-C 后终端恢复正常。
11. **发布**：推一个 tag，检查 4 个产物和 sha256。确认仓库名 `haoyibits/tracebridge`（install.sh 和 README 里的默认值是按 git 作者名假设的），再用 install.sh 从 GitHub 安装一次。

### 阶段 0：通读和规划 ✅（本文档）

### 阶段 1：t32rcl 协议库（最关键）
- 对照 hlinknet.py 的 CommunicationTcp 和 _library.py，逐字节移植 TCP 分帧、连接和同步握手、message id 规则（包括回绕）、请求和响应格式、错误码。
- 高层操作（cmm、PRACTICE 函数求值、symbol 查询、地址解析、print）要找到 RCL 库里真正发出的底层消息，实现同样的消息。
- 测试：
  a) 单元测试：帧编解码、message id 回绕、响应解析。
  b) 在测试内起一个假 RCL TCP 服务端，回放固定字节，覆盖每个公开操作和错误路径。
  c) 写一个抓包工具（tools/ 或 examples/ 下的 TCP 转发器）：让 Python 库经它连接真实 PowerView，按方向落盘成 fixture。我抓到真实流量后，b) 改用真实 fixture。
- 写一个 example 二进制：连接 localhost:20000，执行 cmd、读 system_up、读一段内存。手动验证清单里写明如何跟 Python 库的结果对比。

### 阶段 2：配置和 CLI 骨架
- 移植 config.py：字段、默认值、环境变量覆盖、平台默认值（_host_default）、错误信息都保持一致，路径基准按「新旧差异」修改。
- clap 子命令全部建好，未实现的返回明确错误。实现 init 和 config（输出格式参考 cli.py 的 _print_config）。
- 移植 tests/test_config.py，补充向上查找 toml 和 --config 优先级的测试。

### 阶段 3：open、flash、load
参考 powerview.py、target.py、remote.py、toolbar.cmm，以及对应测试。
- start_powerview：端口已开时先做 RCL 校验，通过就复用，否则报错说明端口被占但不是 RCL。未开时启动 `<t32_binary> -c <t32_config>`，cwd 设为项目目录，stdin 为 null，stdout 和 stderr 追加到 run 目录下的 powerview.log，进程放进新会话（setsid）。macOS 上 t32*-qt 启动器会通过 open 立即以 0 退出，这不算失败。之后轮询端口并做 RCL 校验，超时 120 秒，错误信息照 wait_for_powerview。
- 工具栏：按「新旧差异」第 4 条改写 toolbar.cmm，运行时写到 run 目录，再用 RCL 的 cmm 执行，带 60 秒连接重试。
- target 的命令序列（flash、load、attach、RTOS、Go）逐条照 target.py 移植，顺序不能变。用一个记录命令的测试替身断言完整序列。
- 手动验证清单：open 冷启动、open 复用、flash、load、PowerView 工具栏两个按钮。

### 阶段 4：adapter（DAP 兼容代理，tokio）
参考 dap/protocol.py、dap/proxy.py、tests/test_dap_*.py，行为要逐条对齐。
- 在 dap_port 监听，端口已被占用时照 cli.py 提示并退出。启动 `<debug_adapter> --port <dap_backend_port> --log_to stdout`，T32_DAP_DEBUG=1 时加 `--log_level debug`。后端端口已被占用时报错；连接后端时带重试，超时是 dap_backend_timeout。
- Content-Length 分帧编解码，半包、粘包、非法头的处理跟 protocol.py 一致。
- 拦截逻辑：
  - 客户端发来 `restart`：通过 RCL 执行 Break 和 SYStem.Mode Up，清空 locals 引用集合，向后端发内部请求 `continue {threadId: 0}`，然后回客户端成功响应和 `continued` 事件（allThreadsContinued: true）。失败时回失败响应，附上错误消息。
  - 记录后端 `scopes` 响应里 locals 作用域（presentationHint == "locals" 或 name 小写等于 "locals"）的 variablesReference；客户端请求这些引用的 `variables` 时，直接回空列表。原因见「已定的实现决策」第 4 条，写进注释。
  - 内部请求的响应在代理内部消费，不转发给客户端。
  - 两个方向的写入各自串行化。
- 退出和清理（adapter 进程退出、客户端断开、SIGINT/SIGTERM）跟 run() 和 shutdown() 一致。
- 测试：用假后端和假客户端，覆盖 restart 成功和失败、locals 过滤、分帧边界、后端连不上。

### 阶段 5：rtt
参考 rtt.py，命令行参数和默认值以 add_arguments 为准。
- 通过 symbol 或 control_block_address 定位 SEGGER RTT 控制块。只支持 32 位布局，偏移量照 rtt.py 的常量。检查 "SEGGER RTT" 标识，未初始化时持续等待。
- 通道 0 双向：环形缓冲区读写，包括回绕时的两段拷贝，读写完回写 RdOff/WrOff，内存访问用 "E:"。
- 终端：cfg(unix) 下关闭 ICANON 和 ECHO（VMIN=1，VTIME=0），退出时（包括 panic 和 Ctrl-C）一定要恢复终端设置。
- 开始前检查 RCL 端口，没有时报错，提示信息跟 cli.py 一致。连接成功后在 PowerView 里 print 一行。
- 环形缓冲区逻辑写成纯函数，用模拟内存做单元测试，覆盖回绕、满、空、未初始化。

### 阶段 6：vscode / rustrover（见「最终决定」Q10）
参考 vscode/installer.py、vscode/jsonc.py、模板和对应测试。
- 模板嵌进二进制，占位符按「新旧差异」第 4 条修改，保留 __T32_DAP_PORT__ 和 __T32_RCL_PORT__（注意 replace_tokens：整个字符串等于占位符时替换成原始类型，端口变成数字）。
- JSONC 读取（注释和尾逗号）、跟已有 tasks.json/launch.json 合并（规则照 merge_document，包括 TASK_ALIASES 和 LEGACY_LAUNCH_NAMES）、写入前备份、原子写入，全部跟 Python 版一致。
- 移植 test_vscode_installer.py 和 test_jsonc.py。

### 阶段 7：发布
- GitHub Actions：在 tag 上构建 aarch64-apple-darwin、x86_64-apple-darwin、x86_64-unknown-linux-musl、aarch64-unknown-linux-musl，产物是 tar.gz 加 sha256。先比较 cargo-dist 和手写 workflow，告诉我你的推荐。
- install.sh（curl | sh，装到 ~/.local/bin）。
- README：安装方法、快速开始（init → config → flash → vscode 或 rustrover）、从 Python 版迁移的说明、macOS 从浏览器下载后要处理 quarantine。

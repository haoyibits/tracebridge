# tracebridge

从命令行驱动 Lauterbach TRACE32 PowerView，并在 VS Code 或 RustRover 里用它调试。只有一个静态二进制文件，不依赖 Python。项目里只需要放一个 `trace32.toml`。

*English: [README.md](README.md)*

| 命令 | 作用 |
|---|---|
| `tracebridge init` | 在当前目录生成带注释的 `trace32.toml` |
| `tracebridge config` | 显示解析后的路径和端口，标出缺失项 |
| `tracebridge open` | 启动 PowerView（已在运行则复用） |
| `tracebridge flash` | 用项目的 flash 脚本烧录 ELF，加载符号，运行 |
| `tracebridge load` | 不烧录，只加载符号并运行 |
| `tracebridge rtt` | 双向 SEGGER RTT 终端（Ctrl-C 退出） |
| `tracebridge vscode` | 往 `.vscode/` 写入 `TRACE32: Attach` 调试配置 |
| `tracebridge rustrover` | 往 `.run/` 写入 `TRACE32: Attach` 运行配置 |
| `tracebridge adapter` | `t32debugadapter` 前面的 DAP 代理，由 IDE 自动启动 |

支持的主机：macOS（Apple 芯片、Intel）和 Linux（x86_64、aarch64）。

## 安装

```sh
curl -fsSL https://github.com/haoyibits/tracebridge/releases/latest/download/install.sh | sh
```

默认装到 `~/.local/bin`：用 `TRACEBRIDGE_INSTALL_DIR` 改目录，用 `TRACEBRIDGE_VERSION=v0.1.0` 固定版本。这个目录需要在 `PATH` 里。

**macOS 用浏览器下载时**：下载的文件会被加上隔离属性，Gatekeeper 会拒绝运行。执行一次：

```sh
xattr -d com.apple.quarantine /path/to/tracebridge
```

用 `install.sh`（curl）安装不会带这个属性；如果有，脚本会顺手去掉。

从源码安装：`cargo install --path crates/tracebridge`。

## 前提

- TRACE32 PowerView 和 `t32debugadapter`。默认安装在 `~/t32` 时会自动找到。
- 通过 TCP 访问 Remote API。在 `config.t32` 里写：

  ```text
  RCL=NETTCP
  PORT=20000
  ```

  也可以不写：`config.t32` 里没有 `RCL=` 时，tracebridge 启动 PowerView 会自动加上 `--t32-api-rcl=TCP:<rcl_port>`。

## 快速开始

```sh
cd ~/work/my_app            # 项目根目录
tracebridge init            # 生成 trace32.toml
$EDITOR trace32.toml        # 改 program、elf、[target]、flash.script
tracebridge config          # 每一行都应该是 ok
tracebridge flash           # 启动 PowerView、烧录、加载符号、运行
tracebridge vscode          # 或者：tracebridge rustrover
```

在项目里任意子目录都能运行：和 git 一样，tracebridge 从当前目录往上找最近的 `trace32.toml`。也可以用 `--config <path>` 指定。

日常流程：

1. 用你自己的构建系统编译出 ELF。
2. `tracebridge flash`（只更新符号时用 `tracebridge load`）。PowerView 由 tracebridge 启动时，工具栏上还会多出 **Flash** 和 **Load ELF** 两个按钮。
3. 在 IDE 里调试（见下文），或者直接用 PowerView。
4. `tracebridge rtt` 打开目标板的 RTT 控制台。

## 配置

`trace32.toml` 所在目录就是项目根目录，文件里的相对路径都以它为基准。各节和环境变量见 [README.md](README.md#configuration)。环境变量优先于配置文件，配置文件优先于默认值。几点要注意：

- `PROGRAM_NAME`、`T32_CPU` 这类变量只要设置了，即使是空值也会生效。
- `T32_SYS`、`ELF` 这类变量是空值时会被忽略。
- `T32_FLASH_ARGS` 按 shell 规则拆分成参数。

运行时文件（PowerView 日志、工具栏脚本）写在 `<项目>/.tracebridge/`，这个目录里自带 `.gitignore`。

flash 脚本由项目自己提供，必须支持 `PREPAREONLY` 约定：脚本只初始化目标板、声明 Flash，然后返回。之后由 tracebridge 执行 `FLASH.ReProgram ALL /Erase`、`Data.LOAD.Elf`、`FLASH.ReProgram OFF`、`SYStem.Down`、`SYStem.Up`。

## 在 IDE 里调试

两种 IDE 用的是同一个 DAP 代理（`tracebridge adapter`），调试前 PowerView 必须已经在运行（先执行 `flash`、`load` 或 `open`）。代理额外做两件事：

- **Restart**：通过 Remote API 复位目标，再让程序继续运行。
- **Locals 请求**：直接回空列表，因为部分 `t32debugadapter` 版本在 FreeRTOS 中断栈帧上读 Locals 会崩溃。

Watch、寄存器、调用栈、断点、单步都照常可用。

- **VS Code**：运行 `tracebridge vscode`。它会合并写入 `.vscode/launch.json` 和 `tasks.json`，已有的条目会保留，写之前会备份成 `*.bak.<时间戳>`。然后在 Run and Debug 里选 **TRACE32: Attach**，按 F5。
- **RustRover**：运行 `tracebridge rustrover`。
  1. 安装 **LSP4IJ** 插件（Settings → Plugins → Marketplace），它为 JetBrains IDE 提供 DAP 支持。
  2. 命令会生成 `.run/TRACE32 Attach.run.xml`，IDE 里会出现 **TRACE32: Attach** 运行配置。
  3. 选中它按 **Debug**。
  4. 断点只能打在匹配 `*.c *.h *.cpp *.hpp *.cc *.s *.S *.rs` 的文件里，可以在该配置的 **Mappings** 页修改。

生成的文件里写的是可执行文件和 `trace32.toml` 的绝对路径。项目挪了位置，或 tracebridge 重装到别处之后，重新运行一次这两个命令即可。

## RTT

```sh
tracebridge rtt                 # 通过符号 _SEGGER_RTT 找控制块
tracebridge rtt --cb 0x20000000 # 直接给控制块地址
tracebridge rtt --output-only   # 不转发键盘输入
```

## 从 Python 版 trace32-bridge 迁移

1. 安装 tracebridge。不再需要 Python 和 `lauterbach-trace32-rcl`。
2. 把 `trace32.toml` 从工具包目录移到项目根目录，删掉 `root = ".."`。现在项目根目录就是这个文件所在的目录；文件里如果还有 `project.root`，tracebridge 会报错。
3. `flash.script` 改成相对项目根目录的路径，也可以继续用 `~~/` 开头的 TRACE32 路径。
4. `trace32.sys`、`binary`、`config`、`debug_adapter` 如果写的是相对路径，现在也是相对项目根目录，而不是相对当前目录。
5. 运行 `tracebridge vscode`。它会删掉旧的 `T32: Flash`、`T32: Load ELF`、`T32: RTT Viewer`、`T32: Start Debug Adapter` 四个 task，只留一个隐藏的 adapter task，并更新 `TRACE32: Attach`。烧录、加载和 RTT 改用命令行。
6. 删掉复制进项目的工具包目录（`t32.py`、`trace32_bridge/`、`.run/`）。

其他变化：

- 输出信息和 PowerView 里的提示都改用 `tracebridge` 这个名字。
- 运行时文件放在 `.tracebridge/`。
- `rtt --protocol` 参数删掉了（只支持 TCP）。
- `rtt` 按 Ctrl-C 退出时退出码为 0。

## 许可证

MIT。`crates/t32rcl` 中有从 lauterbach-trace32-rcl 移植的代码（Copyright (c) 2020 Lauterbach GmbH，MIT），见 [NOTICE](NOTICE)。

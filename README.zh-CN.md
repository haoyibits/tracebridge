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
| `tracebridge debug` | 查看寄存器、内存、异常现场，检查板子上是不是刚编译的 ELF，不复位目标 |
| `tracebridge chips <芯片>` | 查看 `flash` 会给这个芯片用哪个烧录脚本 |
| `tracebridge adapter` | `t32debugadapter` 前面的 DAP 代理，由 IDE 自动启动 |

支持的主机：macOS（Apple 芯片、Intel）和 Linux（x86_64、aarch64）。

## 安装

用 Homebrew（macOS 或 Linux）：

```sh
brew install haoyibits/tap/tracebridge
```

或者用安装脚本：

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
$EDITOR trace32.toml        # 改 program、elf、[target]、[flash]
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
5. `tracebridge debug` 查看寄存器、内存和异常现场，或检查板子上跑的是不是刚编译的 ELF。它不会复位目标。

## 配置

`trace32.toml` 所在目录就是项目根目录，文件里的相对路径都以它为基准。各节和环境变量见 [README.md](README.md#configuration)。环境变量优先于配置文件，配置文件优先于默认值。几点要注意：

- `PROGRAM_NAME`、`T32_CPU` 这类变量只要设置了，即使是空值也会生效。
- `T32_SYS`、`ELF` 这类变量是空值时会被忽略。
- `T32_FLASH_ARGS` 按 shell 规则拆分成参数。

运行时文件（PowerView 日志、工具栏脚本）写在 `<项目>/.tracebridge/`，这个目录里自带 `.gitignore`。

**颜色**：在终端里输出带颜色；有没有颜色，文字和列对齐都一样。输出到管道或加了 `--json` 时是纯文本。`NO_COLOR=1` 关闭颜色；`CLICOLOR_FORCE=1` 在管道里也强制带颜色（例如 `tracebridge debug fault | less -R`）。两个例外：`[tracebridge]` 前缀始终是青色；`tracebridge adapter` 的日志始终是纯文本，因为 IDE 要匹配它的内容。

### 烧录脚本怎么选

烧录脚本必须支持 `PREPAREONLY` 约定：脚本只初始化目标板、声明 Flash，然后返回。之后由 tracebridge 执行 `FLASH.ReProgram ALL /Erase`、`Data.LOAD.Elf`、`FLASH.ReProgram OFF`、`SYStem.Down`、`SYStem.Up`。

如果执行 `flash` 时系统已经是 Up 状态，tracebridge 会先执行 `SYStem.Down`。原因是官方烧录脚本只在系统 Down 时才复位并初始化芯片；系统已经 Up 时，烧录算法会直接在应用留下的状态里运行，比如应用已经用 MPU 把算法所在的 RAM 设成不可执行，烧录就会失败。

可以不写脚本路径，只写芯片名：

```toml
[flash]
chip = "STM32H743ZI"    # 留空则用 target.cpu
script = ""             # 写了路径就优先用它
```

tracebridge 按下面的顺序查找：

1. **你的脚本库** `~/.config/tracebridge/flash/*.cmm`。放多个项目共用的脚本，比如 FAE 单独给的、官方发布里没有的脚本，或者改过的官方脚本。
2. **TRACE32 安装目录** `<trace32.sys>/demo/*/flash/*.cmm`，里面有一千多个芯片的脚本。

脚本要被选中，需要同时满足两个条件：头部有能匹配芯片名的 `; @Chip:` 行（允许通配符，如 `STM32H7*`），并且支持 `PREPAREONLY`。有多个脚本匹配时按下面的规则选：

- 精确名字优先于通配符；通配符中字面部分越长越优先。
- 片内 Flash 脚本（如 `stm32f4xx.cmm`）优先于外部存储的变体（`-qspi`、`-spi`、`-emmc`、`-optionbyte` 等）。
- 以上规则分不出高下时会直接报错，这时请在 `flash.script` 里写明路径。

芯片名请填完整型号，也就是 `SYStem.CPU` 用的那个名字，例如 `STM32F407VG`。官方脚本按系列编写，具体型号通过参数 `CPU=<型号>` 传入；不传的话，脚本会退回一个默认型号。所以当选中的脚本支持 `CPU=`、而 `flash.args` 里没有写时，tracebridge 会自动加上 `CPU=<芯片名>`。`DUALPORT=` 等其他参数用脚本自己的默认值，所以 `flash.args` 通常留空。`target.jtag_clock` 由 tracebridge 在脚本执行完后设置。

```sh
tracebridge chips SR6P6              # 看会选哪个脚本
tracebridge flash --chip SR6P6       # 临时指定芯片
tracebridge flash --script <路径>    # 临时指定脚本
```

往脚本库里加脚本：复制过去，并确认头部有一行 `; @Chip: <芯片名>`。修改官方脚本时，请保持它的参数（`PREPAREONLY`、`DUALPORT=` 等）不变，这样脚本库里的版本和原版用法完全一样。许可证写着仅限 TRACE32 使用的脚本不要提交进公开仓库，放在脚本库里就好。

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

## 查看目标状态：`tracebridge debug`

`tracebridge debug` 通过 Remote API，从已经在运行的 PowerView 里读取寄存器、内存和异常现场。它**从不复位目标**：连接时不会发 `SYStem.Up`、`SYStem.Mode Go` 或任何复位命令，也不会启动 PowerView。所以要先用 `tracebridge open`、`flash` 或 `load` 把 PowerView 开起来。

### 两种用法

**交互会话**：只输入 `tracebridge debug`，进入后直接输入命令，不用再带 `tracebridge debug` 前缀。

```text
$ tracebridge debug
[tracebridge] connected to PowerView on RCL port 20000; 'help' lists the commands, 'quit' leaves
t32 [up, running]> break
halted (now: up, halted)
t32 [up, halted]> reg HSCTLR HSCTLR.C
HSCTLR    C15:0x4001        0x30C5183D
          note: defined 4 times in the PER file, all at C15:0x4001; read as '...'
HSCTLR.C  C15:0x4001        0x00000001  "Enabled"
          note: ...
t32 [up, halted]> go
running (now: up, running)
t32 [up, running]> quit
```

- 提示符显示调试器当前的状态，比如 `t32 [up, halted]>`、`t32 [down]>`、`t32 [disconnected]>`。
- Tab 补全命令名，上下箭头翻历史。历史存在项目的 `.tracebridge/debug_history` 里。
- 某条命令出错不会退出会话。连接断了用 `reconnect` 重连。
- `help` 列出全部命令，`help <命令>` 看某条命令的参数。用 `quit`、`exit` 或 Ctrl-D 退出。

**单条命令**：`tracebridge debug <命令> [参数]`，执行一条就退出。适合写进脚本、跑验收检查，或者交给 AI 助手调用。

```sh
tracebridge debug break
tracebridge debug reg HSR
tracebridge debug mem my_buffer 8 --json
tracebridge debug go
```

两种用法的命令和输出完全一样，会话只是省去了每次重新连接和输入前缀。`--json` 让每条命令输出一个 JSON 文档。退出码：0 正常；1 出错（包括寄存器找不到）；2 用法错误；3 表示 `check` 有检查项没通过，或者 `verify` 发现内存和 ELF 不一致。

在终端里输出带颜色：标签和寄存器名是青色，地址蓝色，读到的值加粗，符号黄色，BITFLD 文字和 `ok`/`match` 绿色，`error:`/`FAIL`/`MISMATCH` 和异常原因红色；提示符里 `running` 是绿色，`halted` 是黄色。怎么关闭或强制颜色见[配置](#配置)。

### 典型流程

```text
tracebridge flash                  # 烧录（这一步不属于 debug）
tracebridge debug
t32 [up, running]> verify          # 板子上跑的是刚编译的 ELF 吗？
t32 [up, running]> status          # 模式、运行状态、CPU
t32 [up, running]> break           # 读 CP15 和核心寄存器需要先停核
t32 [up, halted]> status           # 停核后还会显示 PC（符号+偏移）和解码后的 CPSR
t32 [up, halted]> fault            # 程序崩溃后：向量、解码后的 HSR、出错地址
t32 [up, halted]> reg HSR HVBAR    # 按名字读寄存器，也可以写 C15:0x4025
t32 [up, halted]> mem my_buffer 4  # 按符号或地址读内存
t32 [up, halted]> check checks/boot.toml   # 你自己的验收检查
t32 [up, halted]> go
```

### 命令

类型：**R** 只读，不改变任何东西；**S** 会改变目标或调试器的状态；**UI** 只打开 PowerView 窗口。

| 命令 | 类型 | 作用 |
|---|---|---|
| `status` | R | 调试器模式、运行状态、供电、CPU；停核时还显示 PC（符号+偏移）和解码后的 CPSR |
| `reg <名字\|地址>…` | R | 按 PER 文件里的名字（`HSR`）、字段（`HSCTLR.C`，同时显示 BITFLD 的文字含义）、完整 PER 路径或原始地址（`C15:0x4025`、`AD:0x40000000`）读寄存器 |
| `mem <地址\|符号> [个数]` | R | 以十六进制显示若干个 32 位字，默认 1 个 |
| `fault` | R | AArch32 Hyp 异常报告：向量槽位（PC 落在 HVBAR 以外的另一张向量表里时也会指出）、解码后的 HSR、HDFAR 或 HIFAR、带符号的 ELR_hyp、SPSR_hyp。HSR 为 0 时显示没有记录到异常 |
| `eval <表达式>` | R | 求任意 PRACTICE 表达式的值 |
| `verify [elf] [--t32]` | R | 检查目标内存里是不是 ELF 的可加载内容 |
| `check <文件> [--variant V] [--dry-run]` | R | 按数据文件执行验收检查（见下文） |
| `check <文件> --halt` | **S** | 同上，但检查项要读 CP15 或核心寄存器、而核正在运行时，会先停核 |
| `watch <文件\|名字…>` | UI | 打开只包含这些寄存器的 PER.Watch 窗口（需要 PowerView build 176763，即 09/2025 或更新） |
| `attach` | S | `SYStem.Mode Attach`：不复位，核保持原来的运行或停止状态。之后 PowerView 显示的模式是 up |
| `down` | S | `SYStem.Down` |
| `break`、`go` | S | 停核、继续运行 |
| `cmd <PRACTICE 命令>` | S | 执行**任意**命令，包括复位和烧录 |
| `help [命令]`、`reconnect`、`quit` | | 会话命令 |

会话里没有 `up`、`reset`、`flash`，这些请用顶层命令。

### 值是怎么读的

- **一律由 PowerView 求值**：`Data.Long(...)`、`Register(...)`、`PER.VALUE(...)`、`sYmbol.BEGIN(...)`。所以地址和访问类别的含义与 PowerView 命令行完全一致。
- **CP15 和核心寄存器只能在停核时读**。只读命令不会替你停核，只会提示你先执行 `break`。
- **按名字查寄存器**用的是 TRACE32 自己的 `PER.ADDRESS()`、`PER.VALUE()`，查的是这个 CPU 的 PER 文件：
  - `HSR` 按 `.HSR` 搜索；`A.B` 先按 `.A.B`（寄存器.字段）搜索，找不到再当作完整路径。
  - 名字区分大小写。路径元素里有空格时要加引号，并且在 shell 里把整个参数再用单引号括起来：`'"TMR (Timer Unit)".TMR_0.CTRL'`。
  - 点号后面只能跟寄存器名或 `寄存器.字段`，不能跟树名：`.TMR_0.CTRL` 会被 TRACE32 拒绝。像 `CR` 这种在很多外设里都出现的名字，要写从根开始的完整路径。写错时，报错信息会列出匹配的完整路径，可以直接复制。
- **PER 的准备工作**：两步都只改调试器状态、不碰目标，所以算在只读命令里。
  - 执行 `PER.Set.CONDitions`，让 PER 文件里 IF 条件内的寄存器也能被找到；调试器状态变化后会重新执行。
  - 如果报 "No default peripheral file"（由 `tracebridge open` 启动的 PowerView 就是这样），执行一次不带参数的 `PER.ReProgram`，加载 CPU 默认的 PER 文件，并在 stderr 说明，然后重试。每个连接最多执行一次。
- **显示的地址可以直接粘贴到 PowerView 命令行**。`C15:`、`C14:` 寄存器的 `PER.ADDRESS()` 返回的是命令行地址的 4 倍，tracebridge 会换算成命令行的写法，再用 `Data.Long(<地址>)` 读回来自检：
  - 读回的值不同，或者读取失败（比如 bus error）：改为显示 `PER.ADDRESS()` 的原始文本，并附说明。
  - 值是 0 或 0xFFFFFFFF：错误的地址往往也读到这些值，所以标为 unconfirmed（未确认）。
  - `--json` 里的 `address_check` 字段是 `confirmed`、`unconfirmed` 或 `failed`。
- **同一个名字定义了多次**（TRACE32 报 `Ambiguous keyword`）：如果所有定义的地址都相同（字段还要求定义完全相同），就读第一个完整路径，并附一行说明，比如 `defined 4 times in the PER file, all at C15:0x4001`。否则报错，列出各个完整路径和地址，这时请写完整路径或者按地址读。
- **只读的 `rgroup` 条目**：`PER.ADDRESS()` 会报 TRACE32 内部错误 `PAR_256`，但 `PER.VALUE()` 能用。tracebridge 用 `PER.VALUE()` 读值，地址取自 PER 文件，再按上面的方法自检。`check` 文件里也可以直接写这些名字。
- **BITFLD 的文字含义**：先试 TRACE32 的 `PER.VALUE.STRING()`。它在目前试过的字段上都报 "Must be a BITFLD"，所以文字通常取自 PER 文件里这个字段的选项列表，`--json` 里的 `choice_source` 会注明来源。
- 上面用到的路径、地址和选项文字来自对 PER 文件的纯文本扫描，只看 tree、group、标签和字段定义，不解释 `sif`/`if`。**值始终来自 TRACE32**，也就是 PER 函数或 `Data.Long`。

### `verify` 和 `check`

**`verify`** 默认对比 `project.elf`，也可以指定别的 ELF。它把每个带文件内容的 `PT_LOAD` 段，按**加载地址**（LMA，`p_paddr`）和目标内存逐字节比较，而不是按运行地址。所以启动代码会从 NVM 拷到 RAM 的已初始化数据，比较的是 NVM 里的那份。结果是 `match`，或者第一个不同的地址和不同的字节数。`--t32` 会另外跑一次 TRACE32 自己的比较（`Data.LOAD.Elf <elf> /DIFF /PHYSLOAD /NoRegister /NosYmbol /NoClear`，不改内存、PC 和已加载的符号），并报告两者是否一致。

**`check`** 执行放在你项目里的 TOML 检查文件，tracebridge 本身不包含任何板级数据。全部写法见 [`docs/check-example.toml`](docs/check-example.toml)：

```toml
description = "Boot acceptance"

[[check]]
name = "system control"
read = { reg = "SCTRL" }                 # 也可以是 addr = "AD:0x..."、core = "PC"、expr = "..."
expect = { eq = 0x00C50078 }             # eq/ne（可加 mask）、nonzero、range、in_symbol、one_of
variants = ["debug"]                     # 可选：只在 --variant debug 时执行
```

- `eq`、`ne`、`one_of` 里的数字可以写成 `"sym:<符号名>[+偏移]"`。
- 输出是每个检查项一行，最后一行汇总。
- `--dry-run` 只解析所有寄存器名和符号，不读任何寄存器或内存的值，所以核在运行时也能用。
- 有检查项要读 CP15 或核心寄存器、而核正在运行时，`check` 会报错并停止，除非加了 `--halt`。加了 `--halt` 会先执行 `Break` 并说明，检查完核保持停止，用 `tracebridge debug go` 恢复。

### 只放行只读命令（Claude Code）

R 类命令没有副作用，可以让 AI 助手不经询问直接执行。在项目的 `.claude/settings.json` 里写：

```json
{
  "permissions": {
    "allow": [
      "Bash(tracebridge debug status:*)",
      "Bash(tracebridge debug reg:*)",
      "Bash(tracebridge debug mem:*)",
      "Bash(tracebridge debug fault:*)",
      "Bash(tracebridge debug eval:*)",
      "Bash(tracebridge debug verify:*)",
      "Bash(tracebridge debug check checks/boot.toml)",
      "Bash(tracebridge debug check checks/boot.toml --dry-run)",
      "Bash(tracebridge debug check checks/boot.toml --variant release)"
    ]
  }
}
```

- `check` 要用**完整的命令行**放行，不能用 `:*` 前缀，因为前缀规则也会放行会停核的 `check … --halt`。
- `watch`（UI）只打开 PowerView 窗口，也可以加进来。
- 其余命令（`attach`、`down`、`break`、`go`、`cmd`、`check --halt`、`flash`、`load`）仍然需要确认。
- 要让这些规则匹配上，命令要在项目目录里运行（`debug` 前面不要加 `--config`），`--json` 放在命令参数的后面。

## 从 Python 版 trace32-bridge 迁移

1. 安装 tracebridge。不再需要 Python 和 `lauterbach-trace32-rcl`。
2. 把 `trace32.toml` 从工具包目录移到项目根目录，删掉 `root = ".."`。现在项目根目录就是这个文件所在的目录；文件里如果还有 `project.root`，tracebridge 会报错。
3. 把烧录脚本放进脚本库 `~/.config/tracebridge/flash/`（头部要有 `; @Chip:` 行），在配置里写 `flash.chip`。也可以继续用 `flash.script` 写相对项目根目录的路径，或 `~~/` 开头的 TRACE32 路径。
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

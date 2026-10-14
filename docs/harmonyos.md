# HarmonyOS (openharmony) 运行记录

本文件记录在 HarmonyOS PC 上运行本插件原生 worker 的实测结果，以及为让四个文件工具在该平台
可用所做的平台适配。所有数字与命令都是本机实机所得。

## 环境

| 项 | 值 |
| --- | --- |
| 设备 | HarmonyOS PC，HUAWEI KirinX90（20 逻辑核 / 24 GB），HongMeng Kernel 1.13.0 |
| Node | `v26.10.0`，`process.platform` = **`openharmony`**，`process.arch` = `arm64` |
| 目标三元组 | `aarch64-unknown-linux-ohos`（rustc 报告 `target_os="linux"` + `target_env="ohos"`） |
| C 运行库 | musl（`/lib/ld-musl-aarch64.so.1`，无 `/lib/ld-linux-aarch64.so.1`） |
| 宿主 DSH | `0.2.1-alpha.2` |
| 插件 | `0.3.0` |

## 构建

官方 rustup 在本机不可用：aarch64-unknown-linux-gnu 与 static-musl 两版 `rustup-init` 都因缺少
`/lib/ld-linux-aarch64.so.1` 而拒绝执行（`errno=13`）。平台自带的包管理器有原生
`rust 1.99.0`（`arm64_ohos` bottle）：

```console
$ brew install rust
$ rustc -vV | grep host
host: aarch64-unknown-linux-ohos

$ export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER=aarch64-linux-ohos-gcc
$ export RUSTFLAGS="-L native=$(brew --prefix ohos-sdk-native)/llvm/lib/aarch64-linux-ohos"
$ cargo build --release --locked --manifest-path native/Cargo.toml
    Finished `release` profile [optimized] target(s) in 17.64s
```

三处配置都是必需的：本机没有 `cc`（链接器要指向 `aarch64-linux-ohos-gcc`），Rust 的
`aarch64-unknown-linux-ohos` std 会链接 `-lunwind`，而它只在 OHOS 原生 SDK 的
`llvm/lib/aarch64-linux-ohos/` 下。

## 平台限制：`openat2` 会被 seccomp 杀进程

本机应用沙箱的 seccomp 策略**以 `SIGSYS` 终止进程**而不是返回错误，被拒的调用是
`openat2`（437）：

```console
$ ./dsh-ops-worker < request.json
$ echo $?
159            # 128 + SIGSYS(31)，无输出
SIGSYS syscall=437 arch=0xC00000B7 (AUDIT_ARCH_AARCH64)   # 由 SIGSYS 处理器读出
```

逐条实测（每个操作单独进程）：

| 系统调用 | 结果 |
| --- | --- |
| `openat`（绝对路径与 dirfd 相对）、`fstatat`、`statx`、`getdents64`、`renameat2`、`faccessat2`、`getrandom` | 允许 |
| **`openat2`**、`fchmodat2`、`rseq`、`clone3` | **`SIGSYS` 杀进程** |

这与 `cap-std` 直接冲突：在 Linux 目标上它把**所有**目录相对操作实现为 `openat2` +
`RESOLVE_BENEATH`，`stat` 走 `openat2` + `O_PATH` + `fstat`；`rustc` 又把本机目标报成
`target_os="linux"`，所以这条路径被编译进来。库本身在 `openat2` 返回 `ENOSYS` 时有回退，
但这里进程是被杀掉的，回退永远不会执行。

`cap-std` 层面的表现完全一致——**绝对路径操作全部正常，dirfd 相对操作全部被杀**：

| cap-std 操作 | 结果 |
| --- | --- |
| `Dir::open_ambient_dir(abs)`、`Dir::entries()`、`Dir::dir_metadata()`、`std::fs::symlink_metadata(abs)`、`std::fs::File::open(abs)` | 正常 |
| `Dir::open_dir` / `open_dir_nofollow` / `open_with` / `symlink_metadata`（相对）/ `read_dir` / `canonicalize` | `SIGSYS` |

附带发现：应用沙箱**不允许打开文件系统根目录**——`std::fs::read_dir("/")` 与
`Dir::open_ambient_dir("/")` 都是 `EACCES`，`/storage`、`/data`、`/system` 同样，而 `stat`
对所有路径都可用。

## 适配：`target_env = "ohos"` 下的绝对路径后端

`native/src/file_path.rs` 现在按平台分派：

- 其他平台**保持原有受审计的句柄相对遍历**，行为逐字不变；
- `target_env = "ohos"` 下改用绝对路径：`Dir::open_ambient_dir` 打开目录、`entries()` 枚举、
  逐组件 `symlink_metadata` 拒绝链接；打开文件用 `std::fs::File::open` 并与已校验的元数据
  **比对 dev/ino**，因此在校验与打开之间被换成链接的条目仍会被拒绝；
- 锚点不再固定为 `/`：当根目录无法打开时回退到**最深的可打开祖先目录**
  （`file_path::open_anchor`），其余组件仍逐个拒绝链接；
- `file.rs` 的目录遍历与原子替换（`atomic_write`）同样按平台分派；`ohos` 下用
  `create_new` 建临时文件、同目录 `rename` 发布、并 fsync 父目录。

**取舍要说清楚**：ohos 分支放弃了 cap-std 的 dirfd 相对遍历（它天然免疫 TOCTOU），改为
"路径级校验 + 打开后 inode 复核"。这比原实现弱，是有意的平台折衷；其他平台不受影响。

## 实机验证（四文件工具）

在真实 `@deepseek-ai/dsh-subprocess-local` + 真实桥接（`lib/file/worker.js`）下，用
`workspace-write` 会话跑四工具：

| 调用 | 结果 |
| --- | --- |
| `ops_glob` `**/*.txt` | 命中 2 个文件，`complete:true` |
| `ops_grep` `beta`（content / count） | 命中行与逐文件计数均正确 |
| `ops_inspect_local_file`（text / hex） | 逐行返回；hex 视图按 16 字节块 |
| `ops_replace` dryRun | `bytesBefore/After` 与 revision 正确，未写盘 |
| `ops_replace` 实际执行 | `changed:1`，文件内容变为 `alpha\nBETA\ngamma\n`，**目录内无临时文件残留** |
| `ops_replace` in `read-only` | `POLICY_DENIED`（权限门正常） |

安全面复测：

| 场景 | 结果 |
| --- | --- |
| 直接读取指向工作区外的符号链接 | `POLICY_DENIED`（"Links … are not traversed"） |
| 经符号链接目录读取 | `POLICY_DENIED` |
| `ops_glob` 遍历 | 跳过链接，只返回普通文件 |
| 通过符号链接 `ops_replace` | `POLICY_DENIED` |
| `ops_replace` 写工作区与临时区之外（把 `TMPDIR` 挪开后测试） | `POLICY_DENIED`，目标文件未被改动 |

非 ohos 分支的编译验证：本机只装了 ohos std，因此把 `target_env = "ohos"` 等价替换为
一个 feature 后单独 `cargo check`，确认桌面分支仍能编译（`dsh-ops-core` 通过，无警告）。

## 尚未覆盖

- **PTC（`ops_batch` / `ops_result`）**：`native/src/{paths,store,batch}.rs` 仍使用句柄相对
  调用，未做本平台的适配，因此这些工具在本机仍会失败。此外它们还需要宿主
  `sandbox.confine`，而 DSH 的 `dsh-sandbox-local` 平台链只有 `linux` / `darwin` / `win32`，
  `openharmony` 会让 `confine()` 抛 `SandboxUnavailableError`——所以 PTC 只在
  `danger-full-access` 下才可能可用。`ops_bash` 同理。
- 四个文件工具不经过 `sandbox.confine`（`lib/file/worker.js:58-62`），因此三档权限都能用。

## 宿主层

宿主侧（配置、组件开关、提示词注册与撤销、工具注册、manifest 校验）与原生无关，可正常工作：

```console
$ node scripts/validate-manifest.mjs
validate-manifest: all 58 checks passed

$ node test/run.mjs
dsh-ops test: 4 of 14 suite(s) failed, 2 skipped in 5s
```

失败的四项与本平台无关：`permissions` / `ptc` 在模块加载期要求可解析的 worker
（Linux/macOS 同样如此），`binary` / `shells` 是测试里写死的 Windows 期望。

## 其它影响 worker 的本机事实

| 事实 | 实测 | 对 worker 的含义 |
| --- | --- | --- |
| `chmod` 被量化 | 含 user-x → `0770`，否则 `0660`；请求 `0400/0444/0600` 一律得 `0660` | 不能靠 mode 做访问控制；原子替换不必尝试保权限位 |
| 无硬链接 | `link()` → `EPERM` | 原子写只能"临时文件 + rename"（现有实现已是如此） |
| mtime 分辨率 | 约 4 ms（200 次写入仅 12 个不同 mtime） | 变更检测必须用内容哈希，不能靠 mtime |
| CPU 亲和 | 16/20 核可用（`Cpus_allowed_list: 0-15`） | 并行度上限按 16 计 |
| `/tmp` | 只读（erofs） | 临时文件必须写在会话工作区或 `$TMPDIR` |
| 默认 `TMPDIR` | 等于 `$HOME` | 桥接层把 `tmpdir()` 计入可写根，因此"工作区之外"未必是"可写根之外" |

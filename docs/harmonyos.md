# HarmonyOS (openharmony) 运行记录

本文件记录在 HarmonyOS PC 上构建与运行本插件原生 worker 的实测结果：**能构建、能启动，
但受平台 seccomp 策略限制，目录相对（dirfd-relative）文件操作会使进程被 `SIGSYS` 杀死**。
下面每条都附命令与原始输出。

## 环境

| 项 | 值 |
| --- | --- |
| 设备 | HarmonyOS PC，HUAWEI KirinX90（20 逻辑核 / 24 GB） |
| Node | `v26.10.0`，`process.platform` = **`openharmony`**，`process.arch` = `arm64` |
| 目标三元组 | `aarch64-unknown-linux-ohos`（rustc 报告 `target_os="linux"` + `target_env="ohos"`） |
| C 运行库 | **musl**（`/lib/ld-musl-aarch64.so.1`，无 `ld-linux-aarch64.so.1`） |
| 宿主 DSH | `0.2.1-alpha.2` |
| 插件 | `0.3.0`（`c4d829a`） |

`process.platform` 是 `openharmony` 而不是 `linux`，而 rustc 仍把它当作 `target_os="linux"`。
这一对差异同时决定了下面两件事：宿主侧的 `${platform}-${arch}` 目录名匹配不上，以及原生侧
会走进 Linux 专属的快路径。

## 构建：可用（需要的三个额外配置）

官方 rustup 在本机不可用（aarch64 gnu 与 static-musl 两版 `rustup-init` 都因缺少
`/lib/ld-linux-aarch64.so.1` 而拒绝执行，`errno=13`）。平台自带的包管理器有原生
`rust 1.99.0`（`arm64_ohos` bottle）：

```console
$ brew install rust
$ rustc -vV | grep host
host: aarch64-unknown-linux-ohos

$ export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER=aarch64-linux-ohos-gcc
$ export RUSTFLAGS="-L native=$(brew --prefix ohos-sdk-native)/llvm/lib/aarch64-linux-ohos"
$ cargo build --release --locked --manifest-path native/Cargo.toml
    Finished `release` profile [optimized] target(s) in 17.64s

$ file native/target/release/dsh-ops-worker
ELF shared object, 64-bit LSB arm64, dynamic (/lib/ld-musl-aarch64.so.1), not stripped
```

三处配置都是必需的：默认 `cc` 不存在（链接器要指向 `aarch64-linux-ohos-gcc`），Rust 的
`aarch64-unknown-linux-ohos` std 会链接 `-lunwind`，而它只在 OHOS 原生 SDK 的
`llvm/lib/aarch64-linux-ohos/` 下。

## 运行：worker 能启动，但目录相对操作被 seccomp 杀死

worker 启动、读 stdin、解析请求都正常（畸形请求会得到结构化错误）。但任何**以目录句柄为
基准**的文件操作都会让进程收到 `SIGSYS` 并立即终止：

```console
$ printf '{"root":"…","scope":"s","tool":"inspect_local_file","args":{"path":"…/a.txt"}}' \
    | ./native/target/release/dsh-ops-worker
$ echo $?
159            # 128 + SIGSYS(31)，无任何输出
```

用 SIGSYS 处理器读 `siginfo.si_syscall`，被拒绝的系统调用是 **`openat2`（437）**：

```console
SIGSYS syscall=437 arch=0xC00000B7 (AUDIT_ARCH_AARCH64)
```

逐条实测本机策略（每个操作单独进程）：

| 操作 | 结果 |
| --- | --- |
| `openat`（绝对路径 / dirfd 相对）、`fstatat`、`statx`、`getdents64`、`renameat2`、`faccessat2`、`getrandom` | 允许 |
| **`openat2`**、`fchmodat2`、`rseq`、`clone3` | **`SIGSYS` 杀进程** |

原因链：`cap-std` 在 Linux 目标上把**所有**目录相对操作实现为 `openat2` +
`RESOLVE_BENEATH`（`cap-primitives` 的 Linux 后端；`stat` 走 `openat2`+`O_PATH`+`fstat`）。
rustc 把本机目标报成 `target_os="linux"`，所以这条路径被编译进来。库本身在
`openat2` 不可用时（`ENOSYS`）有回退，但这里是**策略杀进程**而不是返回 `ENOSYS`，
因此回退永远不会执行。

附带发现：本机应用沙箱**不允许打开文件系统根目录**——`std::fs::read_dir("/")` 与
`Dir::open_ambient_dir("/")` 都是 `EACCES`，`/storage`、`/data`、`/system` 同样，而
`stat` 对所有路径都可用。原先以 `/` 为锚点的路径解析因此在每个绝对路径上都失败；现在改为
回退到最深的可打开祖先目录（`native/src/file_path.rs` 的 `open_anchor`），组件仍逐个拒绝链接。

**可以作为修复基础的是**：本机一切**绝对路径**操作都正常——`Dir::open_ambient_dir(abs)`、
`Dir::entries()`、`Dir::dir_metadata()`、`std::fs::symlink_metadata(abs)`、
`std::fs::File::open(abs)` 全部实测通过。也就是说，把路径层在 `target_env="ohos"` 下改成
绝对路径 + 逐组件 `lstat` 校验（而不是 dirfd 相对遍历），可以绕开被禁的 `openat2`；这需要
改动 `native/src/{file_path,file,paths,store}.rs`，并明确记录它与现有 no-follow 语义的差异。

## 宿主层：不受上述限制

宿主侧（配置、组件开关、提示词注册与撤销、工具注册、manifest 校验）与原生无关，可正常工作：

```console
$ node scripts/validate-manifest.mjs
validate-manifest: all 58 checks passed

$ node test/run.mjs
dsh-ops test: 4 of 14 suite(s) failed, 2 skipped in 4s
```

失败的四项都不是本平台新增的缺陷：`permissions` / `ptc` 在模块加载期要求可解析的 worker
（Linux/macOS 同样如此，见 #35），`binary` / `shells` 是测试里写死的 Windows 期望
（与 #30 已修的同类）。

另外两组能力需要宿主 `sandbox.confine`，而 DSH 的 `dsh-sandbox-local` 平台链只有
`linux` / `darwin` / `win32`，`openharmony` 会让 `confine()` 抛 `SandboxUnavailableError`：
PTC 的 `ops_batch` / `ops_result` 与 `ops_bash` 因此只在 `danger-full-access` 下可用。
四个原生文件工具不经过 `sandbox.confine`（`lib/file/worker.js:58-62`），所以它们只受上面
那条 `openat2` 限制。

## 其它影响 worker 的本机事实

| 事实 | 实测 | 对 worker 的含义 |
| --- | --- | --- |
| `chmod` 被量化 | 含 user-x → `0770`，否则 `0660`；请求 `0400/0444/0600` 一律得 `0660` | 不能靠 mode 做访问控制；原子替换不必尝试保权限位 |
| 无硬链接 | `link()` → `EPERM` | 原子写只能"临时文件 + rename"（现有实现已是如此） |
| mtime 分辨率 | 约 4 ms（200 次写入仅 12 个不同 mtime） | 变更检测必须用内容哈希，不能靠 mtime |
| seccomp / 能力 | 存在 seccomp filter；`CapEff` 无 `CAP_SYS_ADMIN` | 见上；无法做特权沙箱加强 |
| CPU 亲和 | 16/20 核可用（`Cpus_allowed_list: 0-15`） | 并行度上限按 16 计 |
| `/tmp` | 只读（erofs） | 临时文件必须写在会话工作区或 `$TMPDIR` |

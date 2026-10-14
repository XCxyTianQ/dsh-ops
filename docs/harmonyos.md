# HarmonyOS (openharmony) 运行记录

本文件记录在 HarmonyOS PC 上运行本插件时实测到的边界，以及为了让自建 worker 在这类
平台上可用而做的选择。数字全部来自实机运行，命令与原始输出附在对应小节。

## 环境

| 项 | 值 |
| --- | --- |
| 设备 | HarmonyOS PC，HUAWEI KirinX90（20 逻辑核 / 24 GB） |
| 内核 | HongMeng Kernel 1.13.0，target `linux-aarch64-ohos` |
| Node | `v26.10.0` |
| `process.platform` / `process.arch` | **`openharmony`** / `arm64` |
| C 运行库 | **musl**（`/lib/ld-musl-aarch64.so.1`；无 `/lib/ld-linux-aarch64.so.1`） |
| 宿主 DSH | `0.2.1-alpha.2`（源码树运行） |
| 插件 | `0.3.0`（`c4d829a`） |

`process.platform` 是 `openharmony` 而不是 `linux`，这是本文件存在的原因：任何按
`linux` 前缀判断平台的分支、以及任何 `${platform}-${arch}` 形式的 runtime 目录名，在这台
机器上都不会命中。

## 实测：宿主层可用，原生层需要自己的 worker

`openHarmony` 上**没有**随包 worker 可用，因此四个原生文件工具与 PTC 两个工具都不会发布；
除此之外的宿主层（配置解析、组件开关、提示词注册与撤销、工具注册、manifest 校验）都能
正常工作。

```console
$ node scripts/validate-manifest.mjs
validate-manifest: all 58 checks passed

$ node test/run.mjs
dsh-ops test: 4 of 14 suite(s) failed, 2 skipped in 4s
```

13 个套件（未含本 PR 新增的 `platform`）里 4 个失败，两类原因：

1. **平台门禁**（`lib/ptc/runtime.js` 的 `PLATFORMS`）：`test/permissions.test.mjs:17` 与
   `test/ptc.test.mjs:20` 在模块加载期就要求一个可解析的 worker。前者是崩溃，后者是刻意的
   断言（"this suite never silently skips"）。这在 Linux/macOS 上同样发生，见
   [#35](https://github.com/T-Auto/dsh-ops/issues/35) 与
   [#40](https://github.com/T-Auto/dsh-ops/pull/40) 的 Linux x64 组件。
2. **测试里写死的 Windows 期望**：`test/binary.test.mjs:333`（托管副本优先级）与
   `test/shells.test.mjs` 的 pin 表断言（期望 `C:\home\dsh-ops\shells\pwsh\7.6.6`）。
   这两条与
   [#30](https://github.com/T-Auto/dsh-ops/pull/30) 已修的跨平台期望同类。

其余套件（`bin` `config` `config-updates` `jobs-args` `manifest` `policy` `bash-ipc`）全绿；
`lifecycle` 与 `mount` 按设计跳过（没有 FastCtx 可执行文件）。

## 显式选择在任意平台都生效

`PLATFORMS` 限制的是**自动解析**：随包 runtime 目录、源码构建产物。它不该限制操作者自己
指定的 worker——模块头部声明的契约就是 "explicit selections never silently fall back"，而
一个在未列入平台上的自建 worker 恰恰需要这条路径。现在：

- `config.binaryPath` / `DSH_OPS_WORKER_BIN` 指向的绝对路径**在任意平台**都会被采纳（仍要求
  绝对路径、仍是文件、仍要过收据校验），不再先被平台白名单挡掉；
- 配置了却不可用时**报出该路径**，不会静默落到别的候选；
- staged worker 的收据校验改为"与它所在的 `runtime/<target>/` 目录一致 + 字节与版本一致"，
  这样一份为 Linux ABI 构建、放在 `runtime/linux-arm64/` 的 worker 在 `openharmony-arm64`
  上也能通过校验；
- 自动解析仍然严格要求收据目标等于本机目标，未列入平台仍然失败关闭。

验收：`node test/run.mjs platform`（11 项，任何平台都能跑，不需要 worker 二进制）。

## 在本机跑自建 worker 的步骤

1. 为 **Linux ABI (aarch64)** 构建 worker。注意本机是 musl，不要用 glibc 链接的动态产物：
   静态链接即可（`CGO_ENABLED=0` 的 Go 产物或 `-C target-feature=+crt-static` 的 Rust 产物
   都实测能直接执行）。**官方 rustup 在本机不可用**：它的 aarch64-unknown-linux-gnu 与
   static-musl 两种 `rustup-init` 都因缺少 `/lib/ld-linux-aarch64.so.1` 而拒绝执行
   （`errno=13`）；本机包管理器另有 bottled `rust 1.99.0`，未验证。
2. 放进包的 runtime 布局并写收据（`version` / `target` / `executable` / `sha256`），或直接在
   配置里写 `binaryPath`（或环境变量 `DSH_OPS_WORKER_BIN`）指向它。
3. 会话权限见下节：本机目前只有 `danger-full-access` 能让原生文件工具真正执行。

## 宿主沙箱：本机没有可用的 confinement runner

原生文件工具在 `read-only` / `workspace-write` 下会先经宿主 `sandbox.confine()`，缺少
confinement provider 时**失败关闭**（这是插件的正确行为，不是缺陷）。但在 HarmonyOS 上
宿主本身选不出 runner：DSH 的 `@deepseek-ai/dsh-sandbox-local` 的平台链只有
`linux` / `darwin` / `win32` 三项，未知平台会让 `confine()` 抛 `SandboxUnavailableError`。

因此在本机：**原生工具只在 `danger-full-access` 会话里可用**。若要支持受限模式，需要由
宿主侧提供 confinement——`dsh-sandbox-local` 支持操作者传入 `runnerCommand`（配合
`runnerFailureSignatures`），这属于 DSH 宿主配置，不在本插件范围内。

## 其它影响 worker 的本机事实

| 事实 | 实测 | 对 worker 的含义 |
| --- | --- | --- |
| `chmod` 被量化 | 含 user-x → `0770`，否则 `0660`；请求 `0400/0444/0600` 一律得 `0660` | 不能靠 mode 做访问控制；原子替换不必尝试保权限位 |
| 无硬链接 | `link()` → `EPERM` | 原子写只能"临时文件 + rename"（现有实现已是如此） |
| mtime 分辨率 | 约 4 ms（200 次写入仅 12 个不同 mtime） | 变更检测必须用内容哈希，不能靠 mtime |
| seccomp / 能力 | 存在 seccomp filter，`CapEff` 无 `CAP_SYS_ADMIN` | 无法做特权沙箱加强 |
| CPU 亲和 | 16/20 核可用（`Cpus_allowed_list: 0-15`） | 并行度上限按 16 计 |
| `/tmp` | 只读（erofs） | 临时文件必须写在会话工作区或 `$TMPDIR` |

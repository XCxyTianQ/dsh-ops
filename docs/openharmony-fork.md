# This fork: HarmonyOS (OpenHarmony) and Linux ARM64 support on the 0.3.1 base

Upstream 0.3.1 supports `win32-x64`, `linux-x64` and `darwin-arm64` only, and this
fork adds the two targets our own machines run. Everything here is **additive and
`cfg`-gated**: non-OHOS targets keep upstream's statements verbatim, so a later
rebase onto 0.3.2/0.4 stays cheap.

## What this fork changes

1. **`native/Cargo.toml` + `vendor/cap-primitives`** — HarmonyOS terminates the
   process with `SIGSYS` on `openat2` (measured: `siginfo.si_syscall == 437`), and no
   userspace fallback can observe a signal. The vendored `cap-primitives` (same
   version as upstream's lockfile, 3.4.6) returns `ENOSYS` there instead, which routes
   every caller through the library's own `manually::open` fallback. The guard sits at
   the `openat2`-using function so `stat_impl`, `canonicalize_impl` and
   `open_entry_impl` are covered too. (Approach taken from PR #53, which upstream
   closed as out of the 0.3.1 support matrix.)
2. **`native/src/file_path.rs`** — on OHOS `open("/")` and `open("/storage")` fail with
   `EACCES`/`EPERM` while `stat` works, so upstream's root anchor fails for *every*
   request. The `cfg(target_env = "ohos")` branch walks from the deepest openable
   ancestor prefix and keeps the usual `no-follow` checks for the remaining components.
   (Also from PR #53.)
3. **`native/src/file.rs`** — the ACL-clearing arm of `PreservedMetadata::restore`
   treats any error as fatal ("nothing was published"). On HarmonyOS/hmdfs
   `fremovexattr("system.posix_acl_access")` returns `EPERM`, so **every `replace`
   failed** on the 0.3.1 base. This fork attempts it best-effort on OHOS and reports a
   `metadataWarnings` entry instead. It is safe in this arm: it runs only when the
   original inode carried no access ACL, so nothing is being discarded.
4. **`lib/ptc/runtime.js`** — `PLATFORMS` gains `linux-arm64` and `openharmony-arm64`,
   and an **explicit selection (`binaryPath` / `DSH_OPS_WORKER_BIN`) is consulted
   before the target gate**. Upstream checks the gate first, so on any host outside its
   matrix the documented escape hatch is unreachable and the plugin degrades to zero
   tools (measured on the released 0.3.1: all three configurations refused).
5. **`package.json`** — `os` gains `openharmony` (npm reports
   `{"os":"openharmony","cpu":"arm64"}` on that host). Note that npm's `os`/`cpu` cannot
   express the pairing matrix; the runtime whitelist above remains the real gate.
6. **`test/worker-acceptance.test.mjs`** — our own acceptance suite (worker contract,
   batch contract, bridge policy), adjusted for 0.3.1's persistent update lock.

## What this fork deliberately does *not* change

- `sync_directory`: 0.3.1 already reopens through `/proc/self/fd` when `fsync` on an
  `O_PATH` descriptor returns `EBADF`, and that fallback **works on HarmonyOS**
  (verified: the batch/store suites pass). PR #53's `store.rs`/`file.rs` fsync patches
  are therefore unnecessary on this base.
- `PLATFORMS` gain does not mean the worker is validated everywhere: `linux-arm64` is
  portable Rust and builds/tests on aarch64, while OHOS needs the three changes above.

## Verification on this fork (HarmonyOS, kernel HongMeng 1.13.0, aarch64)

| Check | Result |
| --- | --- |
| `cargo build --release` (OHOS target) | ✓ 21.9 s, worker 3,631,248 B |
| Acceptance suite (file tools / batch contract / bridge policy) | **3/3** ✓ |
| `replace` with metadata restoration | ✓ content changed, `user.hmdfs.perm` preserved, mode/owner reported, ACL warning surfaced |
| `glob` / `grep` / `inspect` on absolute paths | ✓ |

Known OHOS limits that are *not* fixed here: `chmod` is silently ignored by hmdfs, hard
links fail with `EPERM`, and `/tmp` is a read-only erofs (host `TMPDIR` is `$HOME`).

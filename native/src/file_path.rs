//! Absolute host-resolved paths, opened component-by-component with no-follow.
//!
//! ## Platform note: OpenHarmony
//!
//! On OpenHarmony the app sandbox's seccomp policy terminates the process with
//! `SIGSYS` on `openat2` (verified: `siginfo.si_syscall == 437`), and `rustc`
//! reports that target as `target_os = "linux"` / `target_env = "ohos"`. cap-std
//! therefore compiles its Linux fast path, which implements every
//! directory-relative lookup as `openat2` + `RESOLVE_BENEATH` and every `stat`
//! as `openat2` + `O_PATH` + `fstat`. The library's `ENOSYS` fallback cannot
//! help, because the process is killed instead of being handed an error.
//!
//! Absolute-path operations are permitted on that platform, so this module keeps
//! the handle-relative traversal on every other platform and uses absolute paths
//! plus per-component `symlink_metadata` verification under
//! `target_env = "ohos"`. Where a file is opened by path the opened inode is
//! compared against the verified one, so an entry swapped for a link in between
//! is still refused. See `docs/harmonyos.md` for the measurements.
use crate::contract::{Failure, Result};
#[cfg(not(target_env = "ohos"))]
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
#[cfg(windows)]
use cap_fs_ext::OsMetadataExt;
use cap_std::{ambient_authority, fs::Dir};
#[cfg(not(target_env = "ohos"))]
use cap_std::fs::{Metadata, OpenOptions};
use std::path::{Component, Path, PathBuf};

/// One directory entry's metadata, as this platform can obtain it.
#[cfg(target_env = "ohos")]
pub type HostMetadata = std::fs::Metadata;
#[cfg(not(target_env = "ohos"))]
pub type HostMetadata = Metadata;
/// One opened file, as this platform can open it.
#[cfg(target_env = "ohos")]
pub type HostFile = std::fs::File;
#[cfg(not(target_env = "ohos"))]
pub type HostFile = cap_std::fs::File;

pub struct Target { pub parent: Dir, pub name: String, pub display: PathBuf }

/// Whether two metadata records describe the same directory entry.
///
/// Used where an entry had to be opened by name instead of through a verified
/// parent handle: a mismatch means the entry changed identity in between.
#[cfg(target_env = "ohos")]
fn same_entry(verified: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    verified.dev() == opened.dev() && verified.ino() == opened.ino()
}
#[cfg(target_env = "ohos")]
fn swapped() -> Failure {
    Failure::new("ACCESS_DENIED", "The directory entry changed between verification and open")
}

/// One child entry's metadata. The entry is always identified by both the
/// parent handle and the absolute path, so each platform uses the one it can.
pub fn entry_metadata(directory: &Dir, absolute: &Path, name: &str) -> Result<HostMetadata> {
    #[cfg(target_env = "ohos")]
    { let _ = directory; let _ = name; Ok(std::fs::symlink_metadata(absolute)?) }
    #[cfg(not(target_env = "ohos"))]
    { let _ = absolute; Ok(directory.symlink_metadata(name)?) }
}

/// Open one child directory without following links.
pub fn entry_dir(directory: &Dir, absolute: &Path, name: &str) -> Result<Dir> {
    #[cfg(target_env = "ohos")]
    {
        let _ = directory; let _ = name;
        let metadata = std::fs::symlink_metadata(absolute)?;
        regular(&metadata, true)?;
        Dir::open_ambient_dir(absolute, ambient_authority()).map_err(Failure::from)
    }
    #[cfg(not(target_env = "ohos"))]
    { let _ = absolute; Ok(directory.open_dir_nofollow(name)?) }
}

/// Open the target directory itself, after verifying it is not a link.
pub fn target_dir(target: &Target) -> Result<Dir> {
    #[cfg(target_env = "ohos")]
    {
        let metadata = std::fs::symlink_metadata(&target.display)?;
        regular(&metadata, true)?;
        Dir::open_ambient_dir(&target.display, ambient_authority()).map_err(Failure::from)
    }
    #[cfg(not(target_env = "ohos"))]
    { Ok(target.parent.open_dir_nofollow(&target.name)?) }
}

/// Open the deepest ancestor of `names` this process is allowed to open,
/// returning that directory, its absolute path and how many leading names it
/// consumed.
///
/// Desktop platforms let a process open `/`, which is why the root is tried
/// first. Sandboxed platforms do not: on HarmonyOS the app sandbox still
/// answers `symlink_metadata` for every path but refuses to open `/`,
/// `/storage`, `/data` and `/system`, so a root anchor turns every absolute
/// path into `EACCES`. Walking down to the longest openable prefix keeps those
/// platforms usable. Each component folded into the anchor is rejected when it
/// is a link, and every component below the anchor keeps the no-follow descent
/// this module performs for the rest of the path.
fn open_anchor(anchor: &Path, names: &[String]) -> Result<(Dir, PathBuf, usize)> {
    if let Ok(dir) = Dir::open_ambient_dir(anchor, ambient_authority()) {
        return Ok((dir, anchor.to_path_buf(), 0));
    }
    let mut prefix = anchor.to_path_buf();
    for (index, name) in names.iter().enumerate() {
        if index + 1 == names.len() { break }
        prefix.push(name);
        if std::fs::symlink_metadata(&prefix).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(Failure::new("POLICY_DENIED", "Links, reparse points and special objects are not traversed; inspect the target path for the offending entry"));
        }
        if let Ok(dir) = Dir::open_ambient_dir(&prefix, ambient_authority()) {
            return Ok((dir, prefix, index + 1));
        }
    }
    Err(Failure::new("ACCESS_DENIED", "No ancestor directory of the requested path is openable by this process"))
}

pub fn checked(raw: &str) -> Result<Target> {
    let path = Path::new(raw);
    if !path.is_absolute() || raw.len() > 4096 || raw.contains('\0') {
        return Err(Failure::new("INVALID_ARGUMENT", "Expected one absolute, bounded host-resolved path"));
    }
    let mut anchor = PathBuf::new(); let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                #[cfg(windows)]
                if !matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::UNC(_, _)) {
                    return Err(Failure::new("INVALID_ARGUMENT", "Device and verbatim paths are unsupported"));
                }
                anchor.push(prefix.as_os_str());
            }
            Component::RootDir => anchor.push(component.as_os_str()),
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(|| Failure::new("INVALID_ARGUMENT", "Non-UTF8 path"))?;
                validate_name(name)?; names.push(name.to_owned());
            }
            _ => return Err(Failure::new("INVALID_ARGUMENT", "Worker paths must be lexically normalized")),
        }
    }
    let (mut parent, mut walked, anchored) = open_anchor(&anchor, &names)?;
    let mut names = names.split_off(anchored);
    let name = names.pop().unwrap_or_else(|| ".".into());
    for component in names {
        let child = walked.join(&component);
        regular(&entry_metadata(&parent, &child, &component)?, true)?;
        parent = entry_dir(&parent, &child, &component)?;
        walked = child;
    }
    let display = walked.join(&name);
    regular(&entry_metadata(&parent, &display, &name)?, false)?;
    Ok(Target { parent, name, display })
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('\0') {
        return Err(Failure::new("INVALID_ARGUMENT", format!("Invalid path component {name:?}")));
    }
    #[cfg(windows)]
    {
        let base = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
        let port = base.strip_prefix("COM").or_else(|| base.strip_prefix("LPT"));
        if name.ends_with(['.', ' ']) || name.contains([':', '*', '?', '<', '>', '|', '"'])
            || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
            || port.is_some_and(|p| matches!(p, "1"|"2"|"3"|"4"|"5"|"6"|"7"|"8"|"9"|"¹"|"²"|"³"))
        { return Err(Failure::new("INVALID_ARGUMENT", format!("Reserved, ambiguous or device path component {name:?}"))); }
    }
    Ok(())
}
pub fn link(metadata: &HostMetadata) -> bool {
    if metadata.file_type().is_symlink() { return true; }
    #[cfg(windows)]
    if metadata.file_attributes() & 0x400 != 0 { return true; }
    false
}
pub fn regular(metadata: &HostMetadata, directory: bool) -> Result<()> {
    if link(metadata) || (!metadata.is_file() && !metadata.is_dir()) || (directory && !metadata.is_dir()) {
        return Err(Failure::new("POLICY_DENIED", "Links, reparse points and special objects are not traversed; inspect the target path for the offending entry"));
    }
    Ok(())
}
pub fn read_file(target: &Target) -> Result<HostFile> {
    let metadata = entry_metadata(&target.parent, &target.display, &target.name)?;
    regular(&metadata, false)?;
    if !metadata.is_file() { return Err(Failure::new("IS_DIRECTORY", "Expected a regular file; use ops_glob for a directory")); }
    #[cfg(target_env = "ohos")]
    {
        let file = std::fs::File::open(&target.display)?;
        let opened = file.metadata()?;
        regular(&opened, false)?;
        if !same_entry(&metadata, &opened) { return Err(swapped()) }
        Ok(file)
    }
    #[cfg(not(target_env = "ohos"))]
    {
        let mut options = OpenOptions::new(); options.read(true).follow(FollowSymlinks::No);
        let file = target.parent.open_with(&target.name, &options)?;
        regular(&file.metadata()?, false)?;
        Ok(file)
    }
}

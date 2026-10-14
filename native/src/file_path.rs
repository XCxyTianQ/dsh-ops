//! Absolute host-resolved paths, opened component-by-component with no-follow.
use crate::contract::{Failure, Result};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
#[cfg(windows)]
use cap_fs_ext::OsMetadataExt;
use cap_std::{ambient_authority, fs::{Dir, File, Metadata, OpenOptions}};
use std::path::{Component, Path, PathBuf};

pub struct Target { pub parent: Dir, pub name: String, pub display: PathBuf }

/// Open the deepest ancestor of `names` this process is allowed to open,
/// returning that directory and how many leading names it consumed.
///
/// Desktop platforms let a process open `/`, which is why the root is tried
/// first. Sandboxed platforms do not: on HarmonyOS the app sandbox still
/// answers `symlink_metadata` for every path but refuses to open `/`,
/// `/storage`, `/data` and `/system`, so a root anchor turns every absolute
/// path into `EACCES`. Walking down to the longest openable prefix keeps those
/// platforms usable. Each component folded into the anchor is rejected when it
/// is a link, and every component below the anchor keeps the existing
/// no-follow descent; the anchor's own components are the one place this
/// function opens a path it did not verify with a directory handle, and it
/// only reaches that path when opening the root has already failed.
fn open_anchor(anchor: &Path, names: &[String]) -> Result<(Dir, usize)> {
    if let Ok(dir) = Dir::open_ambient_dir(anchor, ambient_authority()) {
        return Ok((dir, 0));
    }
    let mut prefix = anchor.to_path_buf();
    for (index, name) in names.iter().enumerate() {
        if index + 1 == names.len() { break }
        prefix.push(name);
        if std::fs::symlink_metadata(&prefix).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(Failure::new("POLICY_DENIED", "Links, reparse points and special objects are not traversed; inspect the target path for the offending entry"));
        }
        if let Ok(dir) = Dir::open_ambient_dir(&prefix, ambient_authority()) {
            return Ok((dir, index + 1));
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
    let (mut parent, anchored) = open_anchor(&anchor, &names)?;
    let mut names = names.split_off(anchored);
    let name = names.pop().unwrap_or_else(|| ".".into());
    for component in names {
        regular(&parent.symlink_metadata(&component)?, true)?;
        parent = parent.open_dir_nofollow(&component)?;
    }
    regular(&parent.dir_metadata()?, true)?;
    regular(&parent.symlink_metadata(&name)?, false)?;
    Ok(Target { parent, name, display: path.to_path_buf() })
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
pub fn link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() { return true; }
    #[cfg(windows)]
    if metadata.file_attributes() & 0x400 != 0 { return true; }
    false
}
pub fn regular(metadata: &Metadata, directory: bool) -> Result<()> {
    if link(metadata) || (!metadata.is_file() && !metadata.is_dir()) || (directory && !metadata.is_dir()) {
        return Err(Failure::new("POLICY_DENIED", "Links, reparse points and special objects are not traversed; inspect the target path for the offending entry"));
    }
    Ok(())
}
pub fn read_file(target: &Target) -> Result<File> {
    let metadata = target.parent.symlink_metadata(&target.name)?;
    regular(&metadata, false)?;
    if !metadata.is_file() { return Err(Failure::new("IS_DIRECTORY", "Expected a regular file; use ops_glob for a directory")); }
    let mut options = OpenOptions::new(); options.read(true).follow(FollowSymlinks::No);
    let file = target.parent.open_with(&target.name, &options)?;
    regular(&file.metadata()?, false)?;
    Ok(file)
}

//! Materializing a pinned toolchain into the room. Rust only: clone the host
//! rustup toolchain if (and only if) its version matches the spec.

use std::path::{Path, PathBuf};

fn host_rustc_version() -> Result<String, String> {
    let out = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .map_err(|e| format!("running rustc --version: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "rustc --version exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `"rustc 1.98.0 (88d9e12ae 2026-08-18)"` -> `"1.98.0"`.
fn parse_rustc_version(version_line: &str) -> Result<&str, String> {
    version_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("could not parse rustc version line: {version_line}"))
}

fn host_rust_sysroot() -> Result<PathBuf, String> {
    let out = std::process::Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .map_err(|e| format!("running rustc --print sysroot: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "rustc --print sysroot exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Clone the host's rustup toolchain into `<room>/toolchain/rust`. Fails
/// closed if the host `rustc` is not exactly the pinned version — never
/// silently provisions a different compiler than the spec asked for.
pub fn provision_rust(want_version: &str, room: &Path) -> Result<(), String> {
    let version_line = host_rustc_version()?;
    let host_version = parse_rustc_version(&version_line)?;
    if host_version != want_version {
        return Err(format!(
            "host rustc is {host_version}, spec pins {want_version} — refusing to provision a mismatched compiler"
        ));
    }

    let sysroot = host_rust_sysroot()?;
    let dest = room.join("toolchain/rust");
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    clone_dir(&sysroot, &dest)
}

/// APFS clonefile (`cp -cR`) when available; a plain recursive copy otherwise
/// (documented fallback for non-APFS volumes/platforms).
fn clone_dir(src: &Path, dest: &Path) -> Result<(), String> {
    let status = std::process::Command::new("cp")
        .arg("-cR")
        .arg(src)
        .arg(dest)
        .status()
        .map_err(|e| format!("running cp -cR: {e}"))?;
    if status.success() {
        return Ok(());
    }
    copy_recursive(src, dest)
}

fn copy_recursive(src: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_recursive(&from, &to)?;
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(&from).map_err(|e| e.to_string())?;
            std::os::unix::fs::symlink(&target, &to).map_err(|e| e.to_string())?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_version_line() {
        let v = parse_rustc_version("rustc 1.98.0 (88d9e12ae 2026-08-18)").unwrap();
        assert_eq!(v, "1.98.0");
    }
}

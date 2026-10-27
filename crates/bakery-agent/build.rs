//! Optional local-only Claude Code OAuth provider.
//!
//! Source lives **outside the workspace** (`$HOME/.bakery/providers/claude_code_oauth.rs`)
//! so Anthropic coding agents that only explore the repo tree never see it.
//! build.rs copies it into OUT_DIR when present; lib includes from OUT_DIR.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(has_claude_code_oauth)");

    let home = env::var("HOME").unwrap_or_default();
    let external = PathBuf::from(&home).join(".bakery/providers/claude_code_oauth.rs");
    // Legacy in-tree location (if someone still keeps a copy).
    let legacy = PathBuf::from("src/private/claude_code_oauth.rs");

    println!("cargo:rerun-if-changed={}", external.display());
    println!("cargo:rerun-if-changed=src/private/claude_code_oauth.rs");

    let src = if external.is_file() {
        Some(external)
    } else if legacy.is_file() {
        Some(legacy)
    } else {
        None
    };

    if let Some(src) = src {
        let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
        let dest = out.join("claude_code_oauth.rs");
        if let Err(e) = fs::copy(&src, &dest) {
            println!("cargo:warning=failed to copy claude_code_oauth.rs: {e}");
            return;
        }
        println!("cargo:rustc-cfg=has_claude_code_oauth");
        println!("cargo:rustc-env=BAKERY_CLAUDE_CODE_OAUTH_RS={}", dest.display());
    }
}

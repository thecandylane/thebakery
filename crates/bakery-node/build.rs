//! Compiles in the Claude native adapter pin from the files pnpm owns (V1):
//! the exact version in bakery-client/package.json and its integrity in
//! pnpm-lock.yaml. Bumping the package and rebuilding — a Deploy — is the
//! whole Rust side of an upgrade; there is no second copy to forget.

#[path = "src/adapter_pin.rs"]
mod adapter_pin;

use std::path::Path;

const PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";

fn main() {
    let client = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../bakery-client");
    let (manifest, lock) = (client.join("package.json"), client.join("pnpm-lock.yaml"));
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", lock.display());
    let read = |path: &Path| {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    let version = adapter_pin::pinned_version(&read(&manifest), PACKAGE).unwrap_or_else(|| {
        panic!(
            "{}: {PACKAGE} must be pinned to an exact version",
            manifest.display()
        )
    });
    let integrity = adapter_pin::locked_integrity(&read(&lock), PACKAGE, &version)
        .unwrap_or_else(|| {
            panic!(
                "{}: no locked integrity for {PACKAGE}@{version} — run pnpm install in bakery-client",
                lock.display()
            )
        });
    println!("cargo:rustc-env=BAKERY_ADAPTER_VERSION={version}");
    println!("cargo:rustc-env=BAKERY_ADAPTER_INTEGRITY={integrity}");
    // The Bakery's patch to that release, which the seat image applies too,
    // so a container's connector is the host's (empty when pnpm applies none).
    let workspace = client.join("pnpm-workspace.yaml");
    println!("cargo:rerun-if-changed={}", workspace.display());
    let patch = adapter_pin::patch_path(&read(&workspace), PACKAGE, &version)
        .map(|rel| {
            let path = client.join(rel);
            println!("cargo:rerun-if-changed={}", path.display());
            read(&path)
        })
        .unwrap_or_default();
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("claude-agent-acp.patch");
    std::fs::write(&out, patch).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}

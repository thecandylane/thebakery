//! Integration proof for the native-runtime milestone (BAKER-NATIVE-CHAT-CODE-02).
//!
//! Runs the EXACT released Claude adapter+engine over ACP against a loopback
//! mock, with permission prompts ON, and asserts the Review/reject/one-write/
//! isolation properties. Synthetic fixtures only; loopback-only traffic; the
//! operator's real accounts and Keychain are never touched.
//!
//! This spawns a ~200 MB platform engine and reaches the npm registry to
//! assemble the exact-locked runtime, so it is OFF by default and gated behind
//! `BAKERY_NATIVE_DEMO=1`. Run it with:
//!
//! ```sh
//! BAKERY_NATIVE_DEMO=1 cargo test -p bakery-node --test native_runtime_demo -- --nocapture
//! ```
//!
//! The harness itself (fixtures/native/{mock-tools.mjs,acp-driver.mjs,run-demo.sh})
//! is the executable demonstration; this test wires it into `cargo test` and
//! turns its PASS/FAIL into a test result.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn native_adapter_engine_review_reject_isolation() {
    if std::env::var("BAKERY_NATIVE_DEMO").ok().as_deref() != Some("1") {
        eprintln!(
            "SKIP: set BAKERY_NATIVE_DEMO=1 to run the released-engine native demo \
             (spawns the platform engine and assembles the locked adapter runtime)."
        );
        return;
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/native/run-demo.sh");
    assert!(
        script.is_file(),
        "missing demo script at {}",
        script.display()
    );

    // Explicit runs must FAIL on missing deps, assembly errors, driver errors,
    // or timeouts — not silently pass. Strict mode turns the runner's own
    // "unavailable" skips into failures.
    let out = Command::new("sh")
        .arg(&script)
        .env("BAKERY_NATIVE_STRICT", "1")
        .output()
        .expect("failed to launch native demo runner");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("--- run-demo.sh stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    // Under the gate the runner is strict: only a clean PASS is acceptable.
    // Anything else — including an unavailable environment — is a failure,
    // because setting BAKERY_NATIVE_DEMO=1 asserts the runtime is present.
    assert_eq!(
        out.status.code(),
        Some(0),
        "native demo did not pass (exit {:?}):\n{stdout}\n{stderr}",
        out.status.code()
    );
    assert!(
        stdout.contains("NATIVE DEMO: PASS"),
        "runner did not report PASS:\n{stdout}"
    );
}

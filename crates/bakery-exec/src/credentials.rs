//! One place that knows what a credential-shaped filename looks like — used
//! by the files jail (`bakery-node`'s `Jail::read_file`/`grep`), by agent
//! exec's argv check, and by the Seatbelt regex `attach::credential_deny_regex`
//! builds for the agent profile. All three read `CREDENTIAL_SHAPES`; none of
//! them hand-roll their own copy of the name list, so the fence can't drift
//! between "what the Rust check refuses" and "what the kernel refuses."

use std::path::Path;

/// The tool-facing error for any credential-shaped path: never the file
/// contents, never a hint of what the key looks like.
pub const CREDENTIAL_SURFACE_DENIED: &str = "denied: credential surface";

/// Glob shapes (`*` = any run of characters, everything else literal),
/// matched case-insensitively against a whole filename — not a substring, so
/// a shape without a leading/trailing `*` only matches that exact name.
pub const CREDENTIAL_SHAPES: &[&str] = &[
    "secrets.json",
    ".env",
    ".env.*",
    "oauth_creds.json",
    "auth.json",
    "*-auth.json",
    "*credentials*.json",
    "*.pem",
    "*.p12",
    "*.pfx",
];

/// Classic glob match: `*` consumes zero or more characters, everything else
/// must match literally. Both `pattern` and `text` are assumed already
/// lowercased by the caller — this function does no case folding itself.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut match_from) = (None, 0usize);
    while ti < text.len() {
        if pi < pattern.len() && pattern[pi] == text[ti] {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star = Some(pi);
            match_from = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            match_from += 1;
            ti = match_from;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

/// The installed-package folders whose certificate bundles are public:
/// pip's and certifi's `cacert.pem`, grpcio's `roots.pem`, and Xcode's Swift
/// package checkouts (SDWebImage's signing certificate).
pub const PACKAGE_FOLDERS: &[&str] = &["site-packages", "node_modules", "SourcePackages"];

/// A `.pem` inside an installed-package folder: a public certificate bundle
/// a package ships, which its installer writes and its HTTPS reads. The
/// operator's own keys never live there; every other credential-shaped file
/// stays blocked.
pub fn is_package_certificate(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pem"))
        && path.components().any(|c| c.as_os_str().to_str().is_some_and(|c| PACKAGE_FOLDERS.contains(&c)))
}

/// A credential-shaped path an agent's room keeps hidden: not a package's
/// certificate bundle.
pub fn is_hidden_credential(path: &Path) -> bool {
    is_credential_filename(path) && !is_package_certificate(path)
}

/// Matched by filename only, not content — this never has to open the file
/// to decide, and it does not care where the file sits (worktree, room,
/// argv). A copy of a credential file is still a credential file.
pub fn is_credential_filename(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    CREDENTIAL_SHAPES
        .iter()
        .any(|shape| glob_match(shape.as_bytes(), lower.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The operator's decision: a package's certificate bundle is public;
    /// every other credential-shaped file stays hidden.
    #[test]
    fn a_packages_certificate_bundle_is_not_a_secret() {
        for bundle in [
            ".venv/lib/python3.12/site-packages/pip/_vendor/certifi/cacert.pem",
            ".venv/lib/python3.11/site-packages/grpc/_cython/_credentials/roots.pem",
            "web/node_modules/some-sdk/ca.PEM",
            "app/build/ios/SourcePackages/checkouts/SDWebImage/Certificate/SDWebImage Signing Certificate.pem",
        ] {
            assert!(is_package_certificate(Path::new(bundle)) && !is_hidden_credential(Path::new(bundle)), "{bundle}");
        }
        for secret in [
            "certs/id.pem",
            "site-packages-backup.pem",
            ".venv/lib/site-packages/x/.env",
            "node_modules/x/auth.json",
            "build/ios/SourcePackages/checkouts/x/.env",
        ] {
            assert!(is_hidden_credential(Path::new(secret)), "{secret}");
        }
    }

    #[test]
    fn matches_every_named_shape() {
        for name in [
            "secrets.json",
            ".env",
            ".env.local",
            ".env.production",
            "oauth_creds.json",
            "auth.json",
            "service-auth.json",
            "acme-credentials-2026.json",
            "my_credentials.json",
            "id.pem",
            "cert.p12",
            "cert.pfx",
        ] {
            assert!(
                is_credential_filename(Path::new(name)),
                "{name} should match"
            );
            assert!(
                is_credential_filename(Path::new(&format!("nested/dir/{name}"))),
                "{name} nested should match"
            );
        }
    }

    #[test]
    fn matches_regardless_of_case() {
        for name in [
            "SECRETS.JSON",
            ".ENV",
            "Auth.JSON",
            "Service-Auth.Json",
            "CERT.PEM",
        ] {
            assert!(
                is_credential_filename(Path::new(name)),
                "{name} should match case-insensitively"
            );
        }
    }

    #[test]
    fn does_not_match_ordinary_files() {
        for name in [
            "Cargo.toml",
            "README.md",
            "main.rs",
            "environment.ts",
            "authentication.rs",
        ] {
            assert!(
                !is_credential_filename(Path::new(name)),
                "{name} should not match"
            );
        }
    }
}

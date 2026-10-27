//! The door's own certificate authority, for the sign-in hosts it
//! intercepts (Google's, whose tools take no base URL): made once per node,
//! its key kept where no room reads (`~/.bakery/swap/ca.key`, on every
//! room's deny list besides), its certificate where rooms may
//! (`~/.bakery/cache/swap/`), beside a bundle of the public roots and it
//! for tools that read one bundle. A room's request to an intercepted host
//! is answered by the door itself, under a certificate for that host this
//! authority signs, and sent on over real TLS with its stand-ins made real.

use crate::state::AppState;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// The authority's private key: never in a room.
pub(crate) fn key_path(base: &Path) -> PathBuf {
    base.join("swap/ca.key")
}

/// Where rooms find the authority's certificate and the bundle.
pub(crate) fn trust_dir(base: &Path) -> PathBuf {
    base.join("cache/swap")
}

/// The authority's certificate, for tools that add one to their roots
/// (`NODE_EXTRA_CA_CERTS`).
pub(crate) fn ca_pem(base: &Path) -> PathBuf {
    trust_dir(base).join("bakery-door-ca.pem")
}

/// The public roots and the authority's certificate, for tools that read
/// one bundle (`SSL_CERT_FILE` and the rest).
pub(crate) fn bundle(base: &Path) -> PathBuf {
    trust_dir(base).join("ca-bundle.pem")
}

/// The public roots macOS ships for OpenSSL-style tools.
const SYSTEM_ROOTS: &str = "/etc/ssl/cert.pem";

pub(crate) struct Ca {
    key: rcgen::KeyPair,
    cert: rcgen::Certificate,
    /// Each host's TLS, made once.
    leaves: Mutex<HashMap<String, Arc<rustls::ServerConfig>>>,
}

fn ca_params() -> Result<rcgen::CertificateParams, String> {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Bakery door certificate authority");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    Ok(params)
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let fresh = path.with_extension("new");
    let _ = std::fs::remove_file(&fresh);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&fresh)
        .map_err(|e| format!("{}: {e}", fresh.display()))?;
    std::io::Write::write_all(&mut file, text.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&fresh, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The node's authority: its key as it was made the first time, its
/// certificate and the bundle written afresh for rooms.
fn load_or_make(base: &Path) -> Result<Ca, String> {
    let key_file = key_path(base);
    let key = match std::fs::read_to_string(&key_file) {
        Ok(pem) => rcgen::KeyPair::from_pem(&pem).map_err(|e| format!("{}: {e}", key_file.display()))?,
        Err(_) => {
            let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
            write_private(&key_file, &key.serialize_pem())?;
            key
        }
    };
    let cert = ca_params()?.self_signed(&key).map_err(|e| e.to_string())?;
    let dir = trust_dir(base);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let pem = cert.pem();
    std::fs::write(ca_pem(base), &pem).map_err(|e| e.to_string())?;
    let roots = std::fs::read_to_string(SYSTEM_ROOTS).unwrap_or_default();
    std::fs::write(bundle(base), format!("{roots}\n{pem}")).map_err(|e| e.to_string())?;
    Ok(Ca {
        key,
        cert,
        leaves: Mutex::new(HashMap::new()),
    })
}

/// The authority of the node whose home is `base`.
pub(crate) fn ca(base: &Path) -> Result<Arc<Ca>, String> {
    static CAS: OnceLock<Mutex<HashMap<PathBuf, Arc<Ca>>>> = OnceLock::new();
    let mut cas = CAS.get_or_init(Default::default).lock().unwrap();
    if let Some(ca) = cas.get(base) {
        return Ok(ca.clone());
    }
    let ca = Arc::new(load_or_make(base)?);
    cas.insert(base.to_path_buf(), ca.clone());
    Ok(ca)
}

impl Ca {
    /// The authority's certificate, as a client adds it to its roots.
    #[cfg(test)]
    pub(crate) fn der(&self) -> rustls::pki_types::CertificateDer<'static> {
        self.cert.der().clone()
    }

    /// The TLS a room's connection to `host` is answered with: a
    /// certificate for `host` this authority signs, valid a few weeks.
    pub(crate) fn server_for(&self, host: &str) -> Result<Arc<rustls::ServerConfig>, String> {
        if let Some(config) = self.leaves.lock().unwrap().get(host) {
            return Ok(config.clone());
        }
        let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
        let mut params = rcgen::CertificateParams::new(vec![host.to_string()]).map_err(|e| e.to_string())?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        params.distinguished_name.push(rcgen::DnType::CommonName, host);
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let day = |at: chrono::DateTime<chrono::Utc>| {
            use chrono::Datelike;
            rcgen::date_time_ymd(at.year(), at.month() as u8, at.day() as u8)
        };
        let now = chrono::Utc::now();
        params.not_before = day(now - chrono::Duration::days(1));
        params.not_after = day(now + chrono::Duration::days(30));
        let leaf = params
            .signed_by(&key, &self.cert, &self.key)
            .map_err(|e| e.to_string())?;
        let chain = vec![leaf.der().clone(), self.cert.der().clone()];
        let private = rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(chain, private)
            .map_err(|e| e.to_string())?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let config = Arc::new(config);
        self.leaves.lock().unwrap().insert(host.to_string(), config.clone());
        Ok(config)
    }
}

/// A room's CONNECT to an intercepted `host`: the door answers its TLS, and
/// each request on it goes through the swap ([`crate::swap::intercepted`]).
pub(crate) async fn serve(state: AppState, stream_id: String, host: String, upgraded: hyper::upgrade::Upgraded) {
    use hyper_util::rt::TokioIo;
    let config = match ca(&state.base).and_then(|ca| ca.server_for(&host)) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("swap: {host}: {e}");
            return;
        }
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    let Ok(tls) = acceptor.accept(TokioIo::new(upgraded)).await else {
        return;
    };
    let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
        let (state, stream_id, host) = (state.clone(), stream_id.clone(), host.clone());
        async move {
            let req = req.map(axum::body::Body::new);
            Ok::<_, std::convert::Infallible>(crate::swap::intercepted(&state, &stream_id, &host, req).await)
        }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(tls), service)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The authority is made once, its key the operator's alone, and a
    /// client trusting its certificate completes TLS with what it signs for
    /// a host, and with nothing else.
    #[tokio::test]
    async fn the_door_signs_a_hosts_certificate_its_rooms_trust() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let first_ca = load_or_make(base).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(key_path(base)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the key is the operator's alone");
        let first_key = std::fs::read_to_string(key_path(base)).unwrap();
        drop(first_ca);
        let ca = load_or_make(base).unwrap();
        assert_eq!(std::fs::read_to_string(key_path(base)).unwrap(), first_key, "the same key on the next start");
        let pem = std::fs::read_to_string(ca_pem(base)).unwrap();
        assert!(std::fs::read_to_string(bundle(base)).unwrap().ends_with(&pem), "the bundle trusts it");

        let handshake = |host: &'static str, config: Arc<rustls::ServerConfig>| {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(ca.cert.der().clone()).unwrap();
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let client = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
            async move {
                let (near, far) = tokio::io::duplex(1 << 16);
                let server = tokio::spawn(async move { tokio_rustls::TlsAcceptor::from(config).accept(far).await.is_ok() });
                let name = rustls::pki_types::ServerName::try_from(host).unwrap();
                let client = tokio_rustls::TlsConnector::from(Arc::new(client)).connect(name, near).await;
                let _ = server.await;
                client.is_ok()
            }
        };
        let config = ca.server_for("oauth2.googleapis.com").unwrap();
        assert!(Arc::ptr_eq(&config, &ca.server_for("oauth2.googleapis.com").unwrap()), "made once");
        assert!(handshake("oauth2.googleapis.com", config.clone()).await, "trusted for its host");
        assert!(!handshake("www.example.com", config).await, "never for another");
    }
}

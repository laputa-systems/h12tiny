//! Operating-system certificate trust for h12tiny HTTPS clients.
//!
//! The default verifier loads the host system trust store and refreshes it
//! when the store changes. Explicit root stores and caller-provided verifiers
//! stay under caller control and are never merged with system trust.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};

/// Failure to load or refresh the operating-system trust store.
#[derive(Debug)]
pub struct SystemTrustError {
    message: String,
}

impl SystemTrustError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Human-readable reason the store could not be used.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SystemTrustError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "system trust store: {}", self.message)
    }
}

impl std::error::Error for SystemTrustError {}

impl From<SystemTrustError> for TlsError {
    fn from(error: SystemTrustError) -> Self {
        TlsError::General(error.to_string())
    }
}

/// Default HTTPS verifier backed by the host operating-system trust store.
///
/// On Apple platforms this delegates to the native Security.framework
/// evaluation, preserving platform distrust decisions, unless `SSL_CERT_FILE`
/// or `SSL_CERT_DIR` selects explicit PEM roots. An explicit selection takes
/// precedence and behaves like other platforms: it verifies through
/// rustls-webpki, reloads when the selected files change, and fails closed
/// when they are missing, unreadable, empty, or invalid. Elsewhere it verifies
/// against PEM system roots through rustls-webpki and reloads them when the
/// store changes. A failed reload fails the handshake rather than reusing
/// stale trust.
#[derive(Debug)]
pub struct SystemVerifier {
    provider: Arc<CryptoProvider>,
    state: Mutex<State>,
}

#[derive(Debug)]
enum State {
    #[cfg(target_vendor = "apple")]
    Platform {
        verifier: Option<Arc<rustls_platform_verifier::Verifier>>,
    },
    #[cfg(target_vendor = "apple")]
    File {
        cached: Option<CachedVerifier>,
    },
    #[cfg(not(target_vendor = "apple"))]
    Native {
        cached: Option<CachedVerifier>,
    },
}

#[derive(Debug)]
struct CachedVerifier {
    verifier: Arc<rustls::client::WebPkiServerVerifier>,
    fingerprint: TrustFingerprint,
}

impl SystemVerifier {
    /// Creates a verifier that loads system trust on first use.
    ///
    /// Construction never touches the filesystem so infallible client
    /// constructors stay infallible. The first handshake loads the store and
    /// fails closed when it is missing, unreadable, empty, or invalid.
    pub fn new(provider: Arc<CryptoProvider>) -> Self {
        Self {
            provider,
            state: Mutex::new(State::initial()),
        }
    }

    /// Loads system trust now, failing closed when unavailable.
    ///
    /// Builders that return `Result` call this to surface configuration
    /// failures at build time instead of on first use.
    pub fn preload(&self) -> Result<(), SystemTrustError> {
        let mut state = self.state.lock().map_err(|_| {
            SystemTrustError::new("system trust state is unavailable")
        })?;
        state.ensure_loaded(&self.provider)
    }

    fn verifier_for_handshake(&self) -> Result<Arc<dyn ServerCertVerifier>, SystemTrustError> {
        let mut state = self.state.lock().map_err(|_| {
            SystemTrustError::new("system trust state is unavailable")
        })?;
        state.ensure_loaded(&self.provider)?;
        Ok(state.active_verifier())
    }
}

impl State {
    fn initial() -> Self {
        #[cfg(target_vendor = "apple")]
        {
            State::Platform { verifier: None }
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            State::Native { cached: None }
        }
    }

    fn ensure_loaded(&mut self, provider: &Arc<CryptoProvider>) -> Result<(), SystemTrustError> {
        match self {
            #[cfg(target_vendor = "apple")]
            State::Platform { .. } | State::File { .. } => {
                if env_override_active() {
                    if matches!(self, State::Platform { .. }) {
                        *self = State::File { cached: None };
                    }
                    let State::File { cached } = self else {
                        unreachable!("env override uses file trust");
                    };
                    let fingerprint = TrustFingerprint::capture()?;
                    let fresh = match cached.as_ref() {
                        Some(current) if current.fingerprint == fingerprint => return Ok(()),
                        _ => true,
                    };
                    if fresh {
                        let (verifier, fingerprint) =
                            load_native_verifier(provider, fingerprint)?;
                        *cached = Some(CachedVerifier {
                            verifier,
                            fingerprint,
                        });
                    }
                    Ok(())
                } else {
                    if matches!(self, State::File { .. }) {
                        *self = State::Platform { verifier: None };
                    }
                    let State::Platform { verifier } = self else {
                        unreachable!("platform trust uses native evaluation");
                    };
                    if verifier.is_none() {
                        let native =
                            rustls_platform_verifier::Verifier::new(provider.clone()).map_err(
                                |error| {
                                    SystemTrustError::new(format!(
                                        "native verification is unavailable: {error}"
                                    ))
                                },
                            )?;
                        *verifier = Some(Arc::new(native));
                    }
                    Ok(())
                }
            }
            #[cfg(not(target_vendor = "apple"))]
            State::Native { cached } => {
                let fingerprint = TrustFingerprint::capture()?;
                let fresh = match cached.as_ref() {
                    Some(current) if current.fingerprint == fingerprint => return Ok(()),
                    _ => true,
                };
                if fresh {
                    let (verifier, fingerprint) = load_native_verifier(provider, fingerprint)?;
                    *cached = Some(CachedVerifier {
                        verifier,
                        fingerprint,
                    });
                }
                Ok(())
            }
        }
    }

    fn active_verifier(&self) -> Arc<dyn ServerCertVerifier> {
        match self {
            #[cfg(target_vendor = "apple")]
            State::Platform { verifier } => verifier
                .as_ref()
                .expect("system trust is loaded before use")
                .clone()
                as Arc<dyn ServerCertVerifier>,
            #[cfg(target_vendor = "apple")]
            State::File { cached } => cached
                .as_ref()
                .map(|current| current.verifier.clone() as Arc<dyn ServerCertVerifier>)
                .expect("system trust is loaded before use"),
            #[cfg(not(target_vendor = "apple"))]
            State::Native { cached } => cached
                .as_ref()
                .map(|current| current.verifier.clone() as Arc<dyn ServerCertVerifier>)
                .expect("system trust is loaded before use"),
        }
    }
}

impl ServerCertVerifier for SystemVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let verifier = self
            .verifier_for_handshake()
            .map_err(TlsError::from)?;
        verifier.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        let verifier = self
            .verifier_for_handshake()
            .map_err(TlsError::from)?;
        verifier.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        let verifier = self
            .verifier_for_handshake()
            .map_err(TlsError::from)?;
        verifier.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Builds a client configuration that verifies against system trust.
///
/// The verifier refreshes the store as it changes. Explicit roots and
/// caller-provided verifiers must use another constructor.
pub fn system_client_config(
    provider: Arc<CryptoProvider>,
    alpn_protocols: Vec<Vec<u8>>,
    protocol_versions: Option<Vec<&'static rustls::SupportedProtocolVersion>>,
) -> Result<rustls::ClientConfig, SystemTrustError> {
    let verifier = Arc::new(SystemVerifier::new(provider.clone()));
    verifier.preload()?;
    Ok(finish_system_config(
        provider,
        verifier,
        alpn_protocols,
        protocol_versions,
    ))
}

/// Builds a system-trust configuration without touching the filesystem.
///
/// The first handshake loads the store and fails closed when it is missing,
/// unreadable, empty, or invalid. Infallible constructors use this so they
/// stay infallible; fallible builders preload instead.
pub fn system_client_config_lazy(
    provider: Arc<CryptoProvider>,
    alpn_protocols: Vec<Vec<u8>>,
) -> rustls::ClientConfig {
    let verifier = Arc::new(SystemVerifier::new(provider.clone()));
    finish_system_config(provider, verifier, alpn_protocols, None)
}

fn finish_system_config(
    provider: Arc<CryptoProvider>,
    verifier: Arc<SystemVerifier>,
    alpn_protocols: Vec<Vec<u8>>,
    protocol_versions: Option<Vec<&'static rustls::SupportedProtocolVersion>>,
) -> rustls::ClientConfig {
    let builder = rustls::ClientConfig::builder_with_provider(provider);
    let builder = match protocol_versions {
        Some(versions) => builder
            .with_protocol_versions(&versions)
            .expect("requested TLS protocol versions are supported"),
        None => builder
            .with_safe_default_protocol_versions()
            .expect("Graviola supports Rustls safe default protocol versions"),
    };
    let mut config = builder
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = alpn_protocols;
    config
}

/// Builds a client configuration with system trust and mutual TLS identity.
pub fn system_client_config_with_auth(
    provider: Arc<CryptoProvider>,
    alpn_protocols: Vec<Vec<u8>>,
    protocol_versions: Option<Vec<&'static rustls::SupportedProtocolVersion>>,
    certificate_chain: Vec<CertificateDer<'static>>,
    private_key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<rustls::ClientConfig, SystemTrustError> {
    let verifier = Arc::new(SystemVerifier::new(provider.clone()));
    verifier.preload()?;
    let builder = rustls::ClientConfig::builder_with_provider(provider);
    let builder = match protocol_versions {
        Some(versions) => builder.with_protocol_versions(&versions).map_err(|error| {
            SystemTrustError::new(format!("TLS protocol versions are unsupported: {error}"))
        })?,
        None => builder.with_safe_default_protocol_versions().map_err(|error| {
            SystemTrustError::new(format!("TLS protocol versions are unsupported: {error}"))
        })?,
    };
    let mut config = builder
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certificate_chain, private_key)
        .map_err(|error| SystemTrustError::new(format!("client identity is invalid: {error}")))?;
    config.alpn_protocols = alpn_protocols;
    Ok(config)
}

/// Snapshots the current system roots into an explicit store.
///
/// Used when a caller adds anchors to the default trust. The snapshot
/// boundary is the build; later system changes do not affect it.
pub fn system_roots_snapshot(
    provider: &Arc<CryptoProvider>,
) -> Result<rustls::RootCertStore, SystemTrustError> {
    let _ = provider;
    let result = rustls_native_certs::load_native_certs();
    if result.certs.is_empty() {
        let detail = result
            .errors
            .first()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no CA certificates found".to_string());
        return Err(SystemTrustError::new(format!(
            "no usable CA certificates: {detail}"
        )));
    }
    let mut roots = rustls::RootCertStore::empty();
    let _ = roots.add_parsable_certificates(result.certs);
    if roots.is_empty() {
        return Err(SystemTrustError::new(
            "no parsable CA certificates in the system store",
        ));
    }
    Ok(roots)
}

#[cfg(target_vendor = "apple")]
fn env_override_active() -> bool {
    if std::env::var_os(openssl_probe::ENV_CERT_FILE).is_some() {
        return true;
    }
    match std::env::var_os(openssl_probe::ENV_CERT_DIR) {
        Some(dirs) => std::env::split_paths(&dirs).any(|path| !path.as_os_str().is_empty()),
        None => false,
    }
}

fn load_native_verifier(
    provider: &Arc<CryptoProvider>,
    fingerprint: TrustFingerprint,
) -> Result<(Arc<rustls::client::WebPkiServerVerifier>, TrustFingerprint), SystemTrustError> {
    let result = rustls_native_certs::load_native_certs();
    if result.certs.is_empty() {
        let detail = result
            .errors
            .first()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no CA certificates found".to_string());
        return Err(SystemTrustError::new(format!(
            "no usable CA certificates: {detail}"
        )));
    }
    let mut roots = rustls::RootCertStore::empty();
    let (added, _ignored) = roots.add_parsable_certificates(result.certs);
    if roots.is_empty() {
        return Err(SystemTrustError::new(format!(
            "no parsable CA certificates ({added} decoded, none usable)"
        )));
    }
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .map_err(|error| SystemTrustError::new(format!("system verifier failed to build: {error}")))?;
    Ok((verifier, fingerprint))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrustFingerprint {
    env_file: Option<OsString>,
    env_dir: Option<OsString>,
    file: Option<TrackedFile>,
    dirs: Vec<TrackedDir>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrackedFile {
    path: PathBuf,
    modified: Option<SystemTime>,
    size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrackedDir {
    path: PathBuf,
    modified: Option<SystemTime>,
    entries: BTreeMap<OsString, TrackedFile>,
}

impl TrustFingerprint {
    fn capture() -> Result<Self, SystemTrustError> {
        let env_file = std::env::var_os(openssl_probe::ENV_CERT_FILE);
        let env_dir = std::env::var_os(openssl_probe::ENV_CERT_DIR);
        let (file, dirs) = match (&env_file, &env_dir) {
            (None, None) => {
                let found = openssl_probe::probe();
                (found.cert_file, found.cert_dir)
            }
            _ => (
                env_file.as_ref().map(PathBuf::from),
                env_dir
                    .as_ref()
                    .map(|dirs| {
                        std::env::split_paths(dirs)
                            .filter(|path| !path.as_os_str().is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
        };
        let file = file.map(Self::track_file).transpose()?;
        let mut tracked_dirs = Vec::with_capacity(dirs.len());
        for dir in dirs {
            tracked_dirs.push(Self::track_dir(&dir)?);
        }
        Ok(Self {
            env_file,
            env_dir,
            file,
            dirs: tracked_dirs,
        })
    }

    fn track_file(path: PathBuf) -> Result<TrackedFile, SystemTrustError> {
        let metadata = std::fs::metadata(&path).map_err(|error| {
            SystemTrustError::new(format!("trust file {} is unreadable: {error}", path.display()))
        })?;
        if !metadata.is_file() {
            return Err(SystemTrustError::new(format!(
                "trust path {} is not a file",
                path.display()
            )));
        }
        Ok(TrackedFile {
            path,
            modified: metadata.modified().ok(),
            size: metadata.len(),
        })
    }

    fn track_dir(path: &Path) -> Result<TrackedDir, SystemTrustError> {
        let metadata = std::fs::metadata(path).map_err(|error| {
            SystemTrustError::new(format!(
                "trust directory {} is unreadable: {error}",
                path.display()
            ))
        })?;
        if !metadata.is_dir() {
            return Err(SystemTrustError::new(format!(
                "trust path {} is not a directory",
                path.display()
            )));
        }
        let mut entries = BTreeMap::new();
        let listing = std::fs::read_dir(path).map_err(|error| {
            SystemTrustError::new(format!(
                "trust directory {} cannot be listed: {error}",
                path.display()
            ))
        })?;
        for entry in listing {
            let entry = entry.map_err(|error| {
                SystemTrustError::new(format!(
                    "trust directory {} has an unreadable entry: {error}",
                    path.display()
                ))
            })?;
            let entry_path = entry.path();
            let entry_metadata = std::fs::metadata(&entry_path).map_err(|error| {
                SystemTrustError::new(format!(
                    "trust entry {} is unreadable: {error}",
                    entry_path.display()
                ))
            })?;
            if !entry_metadata.is_file() {
                continue;
            }
            entries.insert(
                entry.file_name(),
                TrackedFile {
                    path: entry_path,
                    modified: entry_metadata.modified().ok(),
                    size: entry_metadata.len(),
                },
            );
        }
        Ok(TrackedDir {
            path: path.to_path_buf(),
            modified: metadata.modified().ok(),
            entries,
        })
    }
}

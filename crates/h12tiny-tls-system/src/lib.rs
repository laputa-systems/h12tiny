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

/// Stable classification for a system-trust failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SystemTrustErrorKind {
    /// The verifier or trust state could not be accessed.
    Unavailable,
    /// A configured file or directory could not be read.
    Unreadable,
    /// The trust material was malformed or unusable.
    Invalid,
    /// The store contained no usable certificates.
    Empty,
    /// The requested TLS policy is unsupported by the provider.
    Unsupported,
}

/// Failure to load or refresh the operating-system trust store.
#[derive(Debug)]
pub struct SystemTrustError {
    kind: SystemTrustErrorKind,
    message: String,
    reload: bool,
}

impl SystemTrustError {
    fn new(kind: SystemTrustErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            reload: false,
        }
    }

    fn during_reload(mut self, reload: bool) -> Self {
        self.reload = reload;
        self
    }

    /// Returns the stable failure classification.
    pub fn kind(&self) -> SystemTrustErrorKind {
        self.kind
    }

    /// Reports whether this failure happened while refreshing an already
    /// loaded store rather than during its first load.
    pub fn is_reload(&self) -> bool {
        self.reload
    }

    /// Human-readable reason the store could not be used.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Where a system-trust client configuration takes its roots from.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SystemTrustSource {
    /// Apple Security.framework evaluation, including platform distrust.
    Platform,
    /// PEM files selected by `SSL_CERT_FILE`/`SSL_CERT_DIR` or probing.
    Files {
        file: Option<PathBuf>,
        directories: Vec<PathBuf>,
    },
    /// The platform certificate store loaded by `rustls-native-certs`.
    NativeStore,
}

/// Trust source used by a finished client configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientTrustSource {
    /// Roots supplied explicitly by the caller.
    ExplicitRoots,
    /// Operating-system trust, with the resolved source.
    System(SystemTrustSource),
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
///
/// On Unix, change detection compares device, inode, and change time, so an
/// atomic replacement or a symlink retarget is observed even when modification
/// time and length are preserved. Other platforms compare only modification
/// time and length, so a replacement that preserves both is not detected
/// before the process restarts.
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
            SystemTrustError::new(
                SystemTrustErrorKind::Unavailable,
                "system trust state is unavailable",
            )
        })?;
        state.ensure_loaded(&self.provider)
    }

    /// Reports the trusted source after loading the store.
    ///
    /// On Apple platforms this distinguishes native evaluation from PEM files
    /// selected through `SSL_CERT_FILE` or `SSL_CERT_DIR`.
    pub fn trust_source(&self) -> Result<SystemTrustSource, SystemTrustError> {
        let mut state = self.state.lock().map_err(|_| {
            SystemTrustError::new(
                SystemTrustErrorKind::Unavailable,
                "system trust state is unavailable",
            )
        })?;
        state.ensure_loaded(&self.provider)?;
        Ok(state.source())
    }

    fn verifier_for_handshake(&self) -> Result<Arc<dyn ServerCertVerifier>, SystemTrustError> {
        let mut state = self.state.lock().map_err(|_| {
            SystemTrustError::new(
                SystemTrustErrorKind::Unavailable,
                "system trust state is unavailable",
            )
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
                    let was_loaded = cached.is_some();
                    let fingerprint = TrustFingerprint::capture()
                        .map_err(|error| error.during_reload(was_loaded))?;
                    let fresh = match cached.as_ref() {
                        Some(current) if current.fingerprint == fingerprint => return Ok(()),
                        _ => true,
                    };
                    if fresh {
                        let (verifier, fingerprint) = load_native_verifier(provider, fingerprint)
                            .map_err(|error| error.during_reload(was_loaded))?;
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
                                    SystemTrustError::new(
                                        SystemTrustErrorKind::Unavailable,
                                        format!("native verification is unavailable: {error}"),
                                    )
                                },
                            )?;
                        *verifier = Some(Arc::new(native));
                    }
                    Ok(())
                }
            }
            #[cfg(not(target_vendor = "apple"))]
            State::Native { cached } => {
                let was_loaded = cached.is_some();
                let fingerprint = TrustFingerprint::capture()
                    .map_err(|error| error.during_reload(was_loaded))?;
                let fresh = match cached.as_ref() {
                    Some(current) if current.fingerprint == fingerprint => return Ok(()),
                    _ => true,
                };
                if fresh {
                    let (verifier, fingerprint) = load_native_verifier(provider, fingerprint)
                        .map_err(|error| error.during_reload(was_loaded))?;
                    *cached = Some(CachedVerifier {
                        verifier,
                        fingerprint,
                    });
                }
                Ok(())
            }
        }
    }

    fn source(&self) -> SystemTrustSource {
        #[cfg(target_vendor = "apple")]
        {
            match self {
                State::Platform { .. } => SystemTrustSource::Platform,
                State::File { cached } => match cached {
                    Some(cached) => SystemTrustSource::Files {
                        file: cached
                            .fingerprint
                            .file
                            .as_ref()
                            .map(|tracked| tracked.path.clone()),
                        directories: cached
                            .fingerprint
                            .dirs
                            .iter()
                            .map(|tracked| tracked.path.clone())
                            .collect(),
                    },
                    None => SystemTrustSource::Files {
                        file: None,
                        directories: Vec::new(),
                    },
                },
            }
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            SystemTrustSource::NativeStore
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
    system_client_config_with_source(provider, alpn_protocols, protocol_versions)
        .map(|(config, _)| config)
}

/// Builds a system-trust client configuration and reports the source used.
pub fn system_client_config_with_source(
    provider: Arc<CryptoProvider>,
    alpn_protocols: Vec<Vec<u8>>,
    protocol_versions: Option<Vec<&'static rustls::SupportedProtocolVersion>>,
) -> Result<(rustls::ClientConfig, SystemTrustSource), SystemTrustError> {
    let verifier = Arc::new(SystemVerifier::new(provider.clone()));
    verifier.preload()?;
    let source = verifier.trust_source()?;
    Ok((
        finish_system_config(provider, verifier, alpn_protocols, protocol_versions),
        source,
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
    system_client_config_with_auth_and_source(
        provider,
        alpn_protocols,
        protocol_versions,
        certificate_chain,
        private_key,
    )
    .map(|(config, _)| config)
}

/// Builds a system-trust configuration with a client identity and reports
/// the trust source used.
pub fn system_client_config_with_auth_and_source(
    provider: Arc<CryptoProvider>,
    alpn_protocols: Vec<Vec<u8>>,
    protocol_versions: Option<Vec<&'static rustls::SupportedProtocolVersion>>,
    certificate_chain: Vec<CertificateDer<'static>>,
    private_key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<(rustls::ClientConfig, SystemTrustSource), SystemTrustError> {
    let verifier = Arc::new(SystemVerifier::new(provider.clone()));
    verifier.preload()?;
    let source = verifier.trust_source()?;
    let builder = rustls::ClientConfig::builder_with_provider(provider);
    let builder = match protocol_versions {
        Some(versions) => builder.with_protocol_versions(&versions).map_err(|error| {
            SystemTrustError::new(
                SystemTrustErrorKind::Unsupported,
                format!("TLS protocol versions are unsupported: {error}"),
            )
        })?,
        None => builder.with_safe_default_protocol_versions().map_err(|error| {
            SystemTrustError::new(
                SystemTrustErrorKind::Unsupported,
                format!("TLS protocol versions are unsupported: {error}"),
            )
        })?,
    };
    let mut config = builder
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certificate_chain, private_key)
        .map_err(|error| {
            SystemTrustError::new(
                SystemTrustErrorKind::Invalid,
                format!("client identity is invalid: {error}"),
            )
        })?;
    config.alpn_protocols = alpn_protocols;
    Ok((config, source))
}

/// Builds a Rustls client configuration with system trust or explicit roots.
///
/// This is the shared policy used by both h12tiny clients: Graviola or an
/// explicit provider is selected by the caller, system trust loads eagerly and
/// refreshes on later handshakes, and explicit roots are never merged with it.
pub struct SystemClientConfigBuilder {
    provider: Arc<CryptoProvider>,
    trust: ClientTrust,
    alpn_protocols: Vec<Vec<u8>>,
    protocol_versions: Option<Vec<&'static rustls::SupportedProtocolVersion>>,
    client_auth: ClientAuthentication,
}

enum ClientTrust {
    System,
    Explicit(rustls::RootCertStore),
}

enum ClientAuthentication {
    None,
    Certificates {
        certificate_chain: Vec<CertificateDer<'static>>,
        private_key: rustls::pki_types::PrivateKeyDer<'static>,
    },
}

impl SystemClientConfigBuilder {
    /// Starts with system trust and the supplied ALPN offerings.
    pub fn new(provider: Arc<CryptoProvider>, alpn_protocols: Vec<Vec<u8>>) -> Self {
        Self {
            provider,
            trust: ClientTrust::System,
            alpn_protocols,
            protocol_versions: None,
            client_auth: ClientAuthentication::None,
        }
    }

    /// Replaces the system trust store with an explicit one.
    ///
    /// Replacement never merges with system roots.
    pub fn root_certificates(mut self, roots: rustls::RootCertStore) -> Self {
        self.trust = ClientTrust::Explicit(roots);
        self
    }

    /// Adds one trust anchor, snapshotting system roots first when needed.
    pub fn add_root_certificate(
        mut self,
        certificate: CertificateDer<'static>,
    ) -> Result<Self, SystemTrustError> {
        let roots = match &mut self.trust {
            ClientTrust::Explicit(roots) => roots,
            ClientTrust::System => {
                let snapshot = system_roots_snapshot(&self.provider)?;
                self.trust = ClientTrust::Explicit(snapshot);
                let ClientTrust::Explicit(roots) = &mut self.trust else {
                    unreachable!("trust was just replaced");
                };
                roots
            }
        };
        roots.add(certificate).map_err(|error| {
            SystemTrustError::new(
                SystemTrustErrorKind::Invalid,
                format!("trust anchor is unusable: {error}"),
            )
        })?;
        Ok(self)
    }

    /// Replaces the ALPN offerings sent during TLS negotiation.
    pub fn alpn_protocols(mut self, protocols: impl IntoIterator<Item = Vec<u8>>) -> Self {
        self.alpn_protocols = protocols.into_iter().collect();
        self
    }

    /// Restricts TLS negotiation to exactly these protocol versions.
    pub fn protocol_versions(
        mut self,
        versions: impl IntoIterator<Item = &'static rustls::SupportedProtocolVersion>,
    ) -> Self {
        self.protocol_versions = Some(versions.into_iter().collect());
        self
    }

    /// Configures no TLS client authentication, which is the default.
    pub fn no_client_auth(mut self) -> Self {
        self.client_auth = ClientAuthentication::None;
        self
    }

    /// Configures a certificate chain and private key for mutual TLS.
    pub fn client_auth(
        mut self,
        certificate_chain: Vec<CertificateDer<'static>>,
        private_key: rustls::pki_types::PrivateKeyDer<'static>,
    ) -> Self {
        self.client_auth = ClientAuthentication::Certificates {
            certificate_chain,
            private_key,
        };
        self
    }

    /// Finishes the Rustls configuration.
    pub fn build(self) -> Result<rustls::ClientConfig, SystemTrustError> {
        self.build_with_trust_source().map(|(config, _)| config)
    }

    /// Finishes the Rustls configuration and reports the trust source used.
    pub fn build_with_trust_source(
        self,
    ) -> Result<(rustls::ClientConfig, ClientTrustSource), SystemTrustError> {
        match (self.trust, self.client_auth) {
            (ClientTrust::System, ClientAuthentication::None) => {
                let (config, source) = system_client_config_with_source(
                    self.provider,
                    self.alpn_protocols,
                    self.protocol_versions,
                )?;
                Ok((config, ClientTrustSource::System(source)))
            }
            (
                ClientTrust::System,
                ClientAuthentication::Certificates {
                    certificate_chain,
                    private_key,
                },
            ) => {
                let (config, source) = system_client_config_with_auth_and_source(
                    self.provider,
                    self.alpn_protocols,
                    self.protocol_versions,
                    certificate_chain,
                    private_key,
                )?;
                Ok((config, ClientTrustSource::System(source)))
            }
            (ClientTrust::Explicit(roots), client_auth) => {
                let builder = rustls::ClientConfig::builder_with_provider(self.provider);
                let builder = match self.protocol_versions {
                    Some(versions) => builder.with_protocol_versions(&versions).map_err(
                        |error| {
                            SystemTrustError::new(
                                SystemTrustErrorKind::Unsupported,
                                format!("TLS protocol versions are unsupported: {error}"),
                            )
                        },
                    )?,
                    None => builder.with_safe_default_protocol_versions().map_err(
                        |error| {
                            SystemTrustError::new(
                                SystemTrustErrorKind::Unsupported,
                                format!("TLS protocol versions are unsupported: {error}"),
                            )
                        },
                    )?,
                };
                let mut config = match client_auth {
                    ClientAuthentication::None => {
                        builder.with_root_certificates(roots).with_no_client_auth()
                    }
                    ClientAuthentication::Certificates {
                        certificate_chain,
                        private_key,
                    } => builder
                        .with_root_certificates(roots)
                        .with_client_auth_cert(certificate_chain, private_key)
                        .map_err(|error| {
                            SystemTrustError::new(
                                SystemTrustErrorKind::Invalid,
                                format!("client identity is invalid: {error}"),
                            )
                        })?,
                };
                config.alpn_protocols = self.alpn_protocols;
                Ok((config, ClientTrustSource::ExplicitRoots))
            }
        }
    }
}

/// Snapshots the current system roots into an explicit store.
///
/// Used when a caller adds anchors to the default trust. The snapshot
/// boundary is the build; later system changes do not affect it.
pub fn system_roots_snapshot(
    provider: &Arc<CryptoProvider>,
) -> Result<rustls::RootCertStore, SystemTrustError> {
    let _ = provider;
    require_designated_file_usable(designated_bundle_file().as_deref())?;
    let result = rustls_native_certs::load_native_certs();
    if result.certs.is_empty() {
        let detail = result
            .errors
            .first()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no CA certificates found".to_string());
        return Err(SystemTrustError::new(
            SystemTrustErrorKind::Empty,
            format!("no usable CA certificates: {detail}"),
        ));
    }
    let mut roots = rustls::RootCertStore::empty();
    let _ = roots.add_parsable_certificates(result.certs);
    if roots.is_empty() {
        return Err(SystemTrustError::new(
            SystemTrustErrorKind::Empty,
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

/// Explicit bundle file the aggregate load treats as authoritative.
///
/// `SSL_CERT_FILE` when set, otherwise the probed default certificate file.
/// Discovered directories are supplementary: entry failures there are
/// tolerated, but the designated file must parse to at least one usable root
/// or loading fails closed instead of silently serving a partial store.
fn designated_bundle_file() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(openssl_probe::ENV_CERT_FILE) {
        return Some(PathBuf::from(explicit));
    }
    openssl_probe::probe().cert_file
}

/// Requires the designated bundle file, when one is configured, to parse to
/// at least one usable root. Every `CERTIFICATE` block must base64-decode
/// and admission-test as a CA certificate; the first failure rejects the
/// file. Supplementary directory entries keep the aggregate loader's
/// leniency.
fn require_designated_file_usable(designated: Option<&Path>) -> Result<(), SystemTrustError> {
    let Some(path) = designated else {
        return Ok(());
    };
    let bytes = std::fs::read(path).map_err(|error| {
        SystemTrustError::new(
            SystemTrustErrorKind::Unreadable,
            format!("designated CA file {} is unreadable: {error}", path.display()),
        )
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        SystemTrustError::new(
            SystemTrustErrorKind::Invalid,
            format!("designated CA file {} is not UTF-8 PEM", path.display()),
        )
    })?;
    let mut blocks = 0;
    let mut usable = 0;
    let mut rest = text;
    while let Some(begin) = rest.find("-----BEGIN CERTIFICATE-----") {
        let after_begin = &rest[begin + "-----BEGIN CERTIFICATE-----".len()..];
        let Some(end) = after_begin.find("-----END CERTIFICATE-----") else {
            return Err(SystemTrustError::new(
                SystemTrustErrorKind::Invalid,
                format!(
                    "designated CA file {} has an unterminated CERTIFICATE block",
                    path.display()
                ),
            ));
        };
        blocks += 1;
        let encoded: String = after_begin[..end]
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect();
        let der = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            encoded.as_bytes(),
        )
        .map_err(|error| {
            SystemTrustError::new(
                SystemTrustErrorKind::Invalid,
                format!(
                    "designated CA file {} has an undecodable CERTIFICATE block: {error}",
                    path.display()
                ),
            )
        })?;
        let mut probe = rustls::RootCertStore::empty();
        probe.add(CertificateDer::from(der)).map_err(|error| {
            SystemTrustError::new(
                SystemTrustErrorKind::Invalid,
                format!(
                    "designated CA file {} has an unusable root: {error:?}",
                    path.display()
                ),
            )
        })?;
        usable += 1;
        rest = &after_begin[end + "-----END CERTIFICATE-----".len()..];
    }
    if blocks == 0 || usable == 0 {
        return Err(SystemTrustError::new(
            SystemTrustErrorKind::Empty,
            format!(
                "designated CA file {} contains no usable certificates",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn load_native_verifier(
    provider: &Arc<CryptoProvider>,
    fingerprint: TrustFingerprint,
) -> Result<(Arc<rustls::client::WebPkiServerVerifier>, TrustFingerprint), SystemTrustError> {
    require_designated_file_usable(
        fingerprint
            .file
            .as_ref()
            .map(|tracked| tracked.path.as_path()),
    )?;
    let result = rustls_native_certs::load_native_certs();
    if result.certs.is_empty() {
        let detail = result
            .errors
            .first()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no CA certificates found".to_string());
        return Err(SystemTrustError::new(
            SystemTrustErrorKind::Empty,
            format!("no usable CA certificates: {detail}"),
        ));
    }
    let mut roots = rustls::RootCertStore::empty();
    let (added, _ignored) = roots.add_parsable_certificates(result.certs);
    if roots.is_empty() {
        return Err(SystemTrustError::new(
            SystemTrustErrorKind::Empty,
            format!("no parsable CA certificates ({added} decoded, none usable)"),
        ));
    }
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .map_err(|error| SystemTrustError::new(
        SystemTrustErrorKind::Unavailable,
        format!("system verifier failed to build: {error}"),
    ))?;
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
    identity: Option<FileIdentity>,
}

/// Filesystem identity that survives mtime/size preservation.
///
/// Modification time and length miss atomic replacement (`rename` over the
/// path) and symlink retargeting when the replacement keeps both. The device
/// plus inode names the exact object, and the change time advances on every
/// content or metadata mutation, including ones that preserve mtime and size:
/// `utimensat` can forge mtime/atime but never ctime.
///
/// Both identities are recorded when the path is a symlink: the link itself
/// (retargeting replaces the link) and the fully resolved target (a nested
/// retarget further down the chain replaces the final object while every
/// outer link stays untouched, as when Laputa flips `active` beneath the
/// canonical bundle symlink). A non-symlink records only its own identity.
/// Dangling or looping links record the link identity with no target.
///
/// Non-Unix platforms keep the portable mtime/length fingerprint: atomic
/// replacement or retargeting that preserves both is not detected there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    change_sec: i64,
    change_nsec: i64,
    is_symlink: bool,
    target: Option<ResolvedTarget>,
}

/// Identity of the fully resolved file behind a symlink chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResolvedTarget {
    device: u64,
    inode: u64,
    change_sec: i64,
    change_nsec: i64,
}

#[cfg(unix)]
fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    let link = std::fs::symlink_metadata(path).ok()?;
    if !link.is_symlink() {
        let target = std::fs::metadata(path).ok()?;
        return Some(FileIdentity {
            device: target.dev(),
            inode: target.ino(),
            change_sec: target.ctime(),
            change_nsec: target.ctime_nsec(),
            is_symlink: false,
            target: None,
        });
    }
    let resolved = std::fs::metadata(path).ok().map(|target| ResolvedTarget {
        device: target.dev(),
        inode: target.ino(),
        change_sec: target.ctime(),
        change_nsec: target.ctime_nsec(),
    });
    Some(FileIdentity {
        device: link.dev(),
        inode: link.ino(),
        change_sec: link.ctime(),
        change_nsec: link.ctime_nsec(),
        is_symlink: true,
        target: resolved,
    })
}

#[cfg(not(unix))]
fn file_identity(_path: &Path) -> Option<FileIdentity> {
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrackedDir {
    path: PathBuf,
    modified: Option<SystemTime>,
    identity: Option<FileIdentity>,
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
            SystemTrustError::new(
            SystemTrustErrorKind::Unreadable,
            format!("trust file {} is unreadable: {error}", path.display()),
        )
        })?;
        if !metadata.is_file() {
            return Err(SystemTrustError::new(
                SystemTrustErrorKind::Invalid,
                format!("trust path {} is not a file", path.display()),
            ));
        }
        Ok(TrackedFile {
            path: path.clone(),
            modified: metadata.modified().ok(),
            size: metadata.len(),
            identity: file_identity(&path),
        })
    }

    fn track_dir(path: &Path) -> Result<TrackedDir, SystemTrustError> {
        let metadata = std::fs::metadata(path).map_err(|error| {
            SystemTrustError::new(
                SystemTrustErrorKind::Unreadable,
                format!("trust directory {} is unreadable: {error}", path.display()),
            )
        })?;
        if !metadata.is_dir() {
            return Err(SystemTrustError::new(
                SystemTrustErrorKind::Invalid,
                format!("trust path {} is not a directory", path.display()),
            ));
        }
        let mut entries = BTreeMap::new();
        let listing = std::fs::read_dir(path).map_err(|error| {
            SystemTrustError::new(
                SystemTrustErrorKind::Unreadable,
                format!("trust directory {} cannot be listed: {error}", path.display()),
            )
        })?;
        for entry in listing {
            let entry = entry.map_err(|error| {
                SystemTrustError::new(
                    SystemTrustErrorKind::Unreadable,
                    format!(
                        "trust directory {} has an unreadable entry: {error}",
                        path.display()
                    ),
                )
            })?;
            let entry_path = entry.path();
            let entry_metadata = std::fs::metadata(&entry_path).map_err(|error| {
                SystemTrustError::new(
                    SystemTrustErrorKind::Unreadable,
                    format!("trust entry {} is unreadable: {error}", entry_path.display()),
                )
            })?;
            if !entry_metadata.is_file() {
                continue;
            }
            let entry_identity = file_identity(&entry_path);
            entries.insert(
                entry.file_name(),
                TrackedFile {
                    path: entry_path,
                    modified: entry_metadata.modified().ok(),
                    size: entry_metadata.len(),
                    identity: entry_identity,
                },
            );
        }
        Ok(TrackedDir {
            path: path.to_path_buf(),
            modified: metadata.modified().ok(),
            identity: file_identity(path),
            entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn designated_bundle_failures_are_classified() {
        let directory = std::env::temp_dir().join(format!(
            "h12tiny-tls-system-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();

        let missing = directory.join("missing.pem");
        let error = require_designated_file_usable(Some(&missing)).unwrap_err();
        assert_eq!(error.kind(), SystemTrustErrorKind::Unreadable);

        let malformed = directory.join("malformed.pem");
        std::fs::write(
            &malformed,
            "-----BEGIN CERTIFICATE-----\n!!!not-base64!!!\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        let error = require_designated_file_usable(Some(&malformed)).unwrap_err();
        assert_eq!(error.kind(), SystemTrustErrorKind::Invalid);

        let empty = directory.join("empty.pem");
        std::fs::write(&empty, "").unwrap();
        let error = require_designated_file_usable(Some(&empty)).unwrap_err();
        assert_eq!(error.kind(), SystemTrustErrorKind::Empty);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reload_classification_is_explicit() {
        let first_load =
            SystemTrustError::new(SystemTrustErrorKind::Unavailable, "first load");
        assert!(!first_load.is_reload());
        assert_eq!(first_load.kind(), SystemTrustErrorKind::Unavailable);

        let reload = SystemTrustError::new(SystemTrustErrorKind::Unreadable, "reload")
            .during_reload(true);
        assert!(reload.is_reload());
        assert_eq!(reload.kind(), SystemTrustErrorKind::Unreadable);
    }
}

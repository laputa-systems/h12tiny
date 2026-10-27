#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn in_child() -> bool {
    std::env::var_os("H12TINY_TLS_TEST_CHILD").is_some()
}

pub fn run_child(test_name: &str) {
    let exe = std::env::current_exe().expect("test binary path");
    let status = std::process::Command::new(exe)
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env("H12TINY_TLS_TEST_CHILD", "1")
        .status()
        .expect("child test process runs");
    assert!(
        status.success(),
        "isolated test {test_name} failed in its own process"
    );
}

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "h12tiny-tls-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn set_bundle_file(path: &Path) {
    let path = path.to_str().expect("bundle path is UTF-8");
    unsafe {
        std::env::set_var("SSL_CERT_FILE", path);
        std::env::remove_var("SSL_CERT_DIR");
    }
}

pub fn clear_bundle_env() {
    unsafe {
        std::env::remove_var("SSL_CERT_FILE");
        std::env::remove_var("SSL_CERT_DIR");
    }
}

pub fn write_bundle(dir: &Path, name: &str, pem_certs: &[&str]) -> PathBuf {
    let path = dir.join(name);
    let mut bundle = String::new();
    for cert in pem_certs {
        bundle.push_str(cert);
        if !cert.ends_with('\n') {
            bundle.push('\n');
        }
    }
    std::fs::write(&path, bundle).expect("bundle file");
    path
}

pub struct P384Chain {
    pub root_pem: String,
    pub root_der: Vec<u8>,
    pub issuing_der: Vec<u8>,
    pub intermediate_pem: String,
    pub intermediate_der: Vec<u8>,
    pub cross_signed_der: Vec<u8>,
    pub server_der: Vec<u8>,
    pub server_key_der: Vec<u8>,
    pub intermediate_key_raw: Vec<u8>,
}

pub fn p384_chain() -> P384Chain {
    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
        ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P384_SHA384,
    };

    fn ca_params(name: &str) -> CertificateParams {
        let mut params = CertificateParams::new(vec![]).expect("CA params");
        let mut distinguished = DistinguishedName::new();
        distinguished.push(DnType::CommonName, name);
        params.distinguished_name = distinguished;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        params
    }

    let root_key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("root key");
    let root = CertifiedIssuer::self_signed(ca_params("Test Root"), root_key).expect("root cert");

    let issuing_key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("issuing key");
    let issuing = CertifiedIssuer::signed_by(ca_params("Test Issuing"), issuing_key, &root)
        .expect("issuing cert");

    let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("leaf key");
    let intermediate_key_raw = leaf_key.public_key_raw().to_vec();
    let intermediate = ca_params("Test Leaf CA")
        .signed_by(&leaf_key, &issuing)
        .expect("intermediate cert");
    let cross_signed = ca_params("Test Leaf CA")
        .signed_by(&leaf_key, &root)
        .expect("cross-signed cert");

    let mut server_params =
        CertificateParams::new(vec!["localhost".to_string()]).expect("server params");
    let mut distinguished = DistinguishedName::new();
    distinguished.push(DnType::CommonName, "localhost");
    server_params.distinguished_name = distinguished;
    server_params.is_ca = IsCa::NoCa;
    server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).expect("server key");
    let leaf_issuer = Issuer::new(ca_params("Test Leaf CA"), leaf_key);
    let server = server_params
        .signed_by(&server_key, &leaf_issuer)
        .expect("server cert");

    P384Chain {
        root_pem: root.pem(),
        root_der: root.der().to_vec(),
        issuing_der: issuing.der().to_vec(),
        intermediate_pem: intermediate.pem(),
        intermediate_der: intermediate.der().to_vec(),
        cross_signed_der: cross_signed.der().to_vec(),
        server_der: server.der().to_vec(),
        server_key_der: server_key.serialize_der(),
        intermediate_key_raw,
    }
}

pub const DISTRUSTED_NAMES: [&str; 8] = [
    "EMDVTLSCAG3B",
    "EMDVTLSCAG3C",
    "EMEVTLSCAG3B",
    "EMEVTLSCAG3C",
    "EMOVTLSCAG3B",
    "EMOVTLSCAG3C",
    "EMOVCSCAG3B",
    "EMOVCSCAG3C",
];

pub const DISTRUSTED_SHA256: [&str; 8] = [
    "2d5f988ee3ceaf696845a34299ca3e05816429cdb5de92427889533175f97b95",
    "616e9a92539c40f454813f7fb8ae4a6db719498495215774f02d8b8b5a615d36",
    "bdf5ab18303069ebc3e3ddbb0fc0613693acef01b2fb7be1a0624aab60fbff0d",
    "b941c7577b918f7b9a9374aff3b02d8eda92545c86adb5b5268d1e121fe58ce2",
    "7f09de2d155cc9fbc2dc4585f2c134ea3fc04ea9d9b3e31fa2d6bf9fdc0a02a2",
    "6fc13706463e00b2cd2ae357f86de2441e59d501e322c7a02a531a9871e450f6",
    "e8263cea915a92109a29fb497105dceeeb58fa099c2552c4082aff1e1a9a33b2",
    "09f75d128061fcdc6d31d8317ce3fe0aea4dab2c66777ce548858dd23f763ce7",
];

pub fn distrusted_spki(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("distrusted")
        .join(format!("{name}.spki.der"));
    std::fs::read(&path).expect("distrusted SPKI fixture")
}

pub fn check_distrusted_fixtures() {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for (name, expected) in DISTRUSTED_NAMES.iter().zip(DISTRUSTED_SHA256.iter()) {
        let bytes = distrusted_spki(name);
        assert_eq!(bytes.len(), 118, "distrusted SPKI {name} length");
        let digest = sha256_hex(&bytes);
        assert_eq!(&digest, expected, "distrusted SPKI {name} hash");
        assert!(seen.insert(digest), "duplicate distrusted SPKI {name}");
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use graviola::hashing::{Hash, HashContext, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finish();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest.as_ref() {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 15) as usize] as char);
    }
    out
}

/// Replaces the 118-byte P-384 SPKI contents in `cert_der` with `replacement`.
///
/// The replacement must be exactly 118 bytes so surrounding DER lengths stay
/// valid. The patched certificate keeps its subject, issuer, and extensions;
/// its signature no longer verifies, which is irrelevant for distrust tests
/// because the verifier checks distrust before signatures.
pub fn patch_spki_contents(cert_der: &[u8], public_key_raw: &[u8], replacement: &[u8]) -> Vec<u8> {
    assert_eq!(replacement.len(), 118, "replacement SPKI length");
    let offset = cert_der
        .windows(public_key_raw.len())
        .position(|window| window == public_key_raw)
        .expect("public key appears in its certificate");
    let spki_start = offset
        .checked_sub(21)
        .expect("SPKI header precedes the public key");
    let spki_end = spki_start + 118;
    assert!(spki_end <= cert_der.len(), "SPKI lies within the certificate");
    let mut patched = cert_der.to_vec();
    patched[spki_start..spki_end].copy_from_slice(replacement);
    patched
}

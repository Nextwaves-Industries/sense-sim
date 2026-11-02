//! TLS trust by pinned public key, for devices with self-signed certificates.
//!
//! PLCs rarely chain to a public CA and are usually addressed by IP, so normal name and
//! chain checks can't apply. Turning verification off, as Siemens' Python example does,
//! would let anyone on the network collect the password. Instead, trust comes down to one
//! question: does this server hold the private key for the public key we pinned?
//!
//! Both halves of that question matter. A certificate is public, so an impostor can present
//! the real one, and only the handshake signature proves it holds the key. Expiry and
//! hostnames are deliberately not checked: PLC clocks are often wrong, and the pin already
//! says exactly which key we trust.

use std::fmt;
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{self, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha256};

/// SHA-256 of a certificate's SubjectPublicKeyInfo. Survives certificate renewal when the
/// key is kept, and is what `SenseTwin webapi-cert` prints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SpkiPin([u8; 32]);

impl SpkiPin {
    pub fn of_cert(cert_der: &[u8]) -> Result<Self, String> {
        let (_, cert) = x509_parser::parse_x509_certificate(cert_der).map_err(|e| format!("unreadable certificate: {e}"))?;
        Ok(Self(Sha256::digest(cert.tbs_certificate.subject_pki.raw).into()))
    }

    /// Parses `sha256:<64 hex digits>`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let hex = text.strip_prefix("sha256:").ok_or("a pin starts with sha256:")?;
        if hex.len() != 64 || !hex.is_ascii() {
            return Err("a pin has 64 hex digits after sha256:".into());
        }
        let mut pin = [0u8; 32];
        for (i, byte) in pin.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| "a pin has only hex digits")?;
        }
        Ok(Self(pin))
    }
}

impl fmt::Display for SpkiPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sha256:")?;
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(crypto::ring::default_provider())
}

fn client_config(verifier: Arc<dyn ServerCertVerifier>) -> Result<Arc<ClientConfig>, Error> {
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Client settings that only complete a handshake with the holder of the pinned key.
pub fn pinned_client(pin: SpkiPin) -> Result<Arc<ClientConfig>, Error> {
    client_config(Arc::new(PinnedKey { pin, provider: provider() }))
}

/// Client settings that note the server's certificate and then abort the handshake, so
/// no request, and therefore no credential, can ever be sent over the connection.
pub fn capturing_client() -> Result<(Arc<ClientConfig>, Arc<CapturedCert>), Error> {
    let capture = Arc::new(CapturedCert::default());
    Ok((client_config(capture.clone())?, capture))
}

#[derive(Debug)]
struct PinnedKey {
    pin: SpkiPin,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedKey {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, Error> {
        let seen = SpkiPin::of_cert(end_entity).map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
        if seen == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[derive(Debug, Default)]
pub struct CapturedCert(Mutex<Option<Vec<u8>>>);

impl CapturedCert {
    pub fn take(&self) -> Option<Vec<u8>> {
        self.0.lock().unwrap().take()
    }
}

impl ServerCertVerifier for CapturedCert {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, Error> {
        *self.0.lock().unwrap() = Some(end_entity.to_vec());
        Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
    }

    // Never reached: `verify_server_cert` always aborts first.
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
    }

    fn verify_tls13_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider().signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZEROS: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn pin_text_round_trips() {
        let text = format!("sha256:{}", "ab".repeat(32));
        assert_eq!(SpkiPin::parse(&text).unwrap().to_string(), text);
        assert_eq!(SpkiPin::parse(ZEROS).unwrap().to_string(), ZEROS);
    }

    #[test]
    fn bad_pin_text_is_rejected() {
        for bad in ["", "sha256:", "md5:00", "abcd", &format!("sha256:{}", "a".repeat(63)), &format!("sha256:{}", "a".repeat(65)), &format!("sha256:{}", "zz".repeat(32)), &format!("sha256:{}é", "a".repeat(62))] {
            assert!(SpkiPin::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn garbage_is_not_a_certificate() {
        assert!(SpkiPin::of_cert(b"not a certificate").is_err());
    }
}

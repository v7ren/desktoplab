//! This device's self-signed keypair. The private key is written so only the
//! current user can read it.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use rcgen::{CertificateParams, DnType, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Msg(String),
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    /// Lowercase SHA-256 of the certificate DER.
    pub fingerprint: String,
}

impl Identity {
    pub fn generate(name: &str) -> Result<Self, IdentityError> {
        let key = KeyPair::generate().map_err(|e| IdentityError::Msg(e.to_string()))?;
        let mut params = CertificateParams::new(vec!["devhop".into()])
            .map_err(|e| IdentityError::Msg(e.to_string()))?;
        params.distinguished_name.push(DnType::CommonName, name);
        let cert = params
            .self_signed(&key)
            .map_err(|e| IdentityError::Msg(e.to_string()))?;
        let cert_der = cert.der().to_vec();
        let key_der = key.serialize_der();
        Ok(Self {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            fingerprint: fingerprint(&cert_der),
            cert_der,
            key_der,
        })
    }

    pub fn load_or_create(dir: &Path) -> Result<Self, IdentityError> {
        fs::create_dir_all(dir)?;
        let cert_path = dir.join("identity.crt");
        let key_path = dir.join("identity.key");
        if cert_path.exists() && key_path.exists() {
            let cert_pem = fs::read_to_string(&cert_path)?;
            let key_pem = fs::read_to_string(&key_path)?;
            return Self::from_pem(&cert_pem, &key_pem);
        }
        let identity = Self::generate("devhop")?;
        write_secret(&key_path, identity.key_pem.as_bytes())?;
        fs::write(&cert_path, identity.cert_pem.as_bytes())?;
        let _ = restrict_to_user(&key_path);
        Ok(identity)
    }

    pub fn from_pem(cert_pem: &str, key_pem: &str) -> Result<Self, IdentityError> {
        let mut cursor = Cursor::new(cert_pem.as_bytes());
        let certs: Vec<_> = rustls_pemfile::certs(&mut cursor)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| IdentityError::Msg(e.to_string()))?;
        let cert_der = certs
            .into_iter()
            .next()
            .ok_or_else(|| IdentityError::Msg("certificate pem is empty".into()))?
            .as_ref()
            .to_vec();
        let key = rustls_pemfile::private_key(&mut Cursor::new(key_pem.as_bytes()))
            .map_err(|e| IdentityError::Msg(e.to_string()))?
            .ok_or_else(|| IdentityError::Msg("private key pem is empty".into()))?;
        let key_der = key.secret_der().to_vec();
        Ok(Self {
            fingerprint: fingerprint(&cert_der),
            cert_pem: cert_pem.to_string(),
            key_pem: key_pem.to_string(),
            cert_der,
            key_der,
        })
    }

    pub fn certificate(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert_der.clone())
    }

    pub fn private_key(&self) -> Result<PrivateKeyDer<'static>, IdentityError> {
        PrivateKeyDer::try_from(self.key_der.clone())
            .map_err(|e| IdentityError::Msg(format!("private key: {e}")))
    }
}

pub fn fingerprint(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

fn write_secret(path: &Path, bytes: &[u8]) -> Result<(), IdentityError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        use std::io::Write;
        opts.open(path)?.write_all(bytes)?;
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        fs::write(path, bytes)?;
        Ok(())
    }
}

fn restrict_to_user(path: &Path) -> Result<(), IdentityError> {
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "Users".into());
        let status = std::process::Command::new("icacls")
            .arg(path)
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(format!("{user}:(F)"))
            .output()?;
        if !status.status.success() {
            return Err(IdentityError::Msg(
                String::from_utf8_lossy(&status.stderr).trim().to_string(),
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_reload_keep_the_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let first = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(first.fingerprint.len(), 64);
        let second = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert!(dir.path().join("identity.key").exists());
        assert!(dir.path().join("identity.crt").exists());
    }
}

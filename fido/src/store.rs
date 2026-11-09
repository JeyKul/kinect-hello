//! Credential storage and signing keys.
//!
//! With the TPM backend each credential gets its own P-256 key created inside the TPM under an
//! ECC storage primary derived from the owner seed. Only the TPM-encrypted private blob is
//! stored on disk; it can be loaded and used for signing on this TPM only.
//! The software backend keeps plain keys and exists for tests and development.

use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum KeyBlob {
    Tpm { private: String, public: String },
    Soft { secret: String },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Credential {
    pub id: String, // hex
    pub rp_id: String,
    pub user_id: String, // hex
    pub user_name: String,
    pub display_name: String,
    pub resident: bool,
    pub created: u64,
    pub key: KeyBlob,
}

impl Credential {
    pub fn id_bytes(&self) -> Vec<u8> {
        hex::decode(&self.id).unwrap_or_default()
    }
    pub fn user_id_bytes(&self) -> Vec<u8> {
        hex::decode(&self.user_id).unwrap_or_default()
    }
}

pub struct PublicKey {
    pub x: [u8; 32],
    pub y: [u8; 32],
}

pub enum Backend {
    Tpm(tpm::Tpm),
    Soft,
}

pub struct Store {
    dir: PathBuf,
    backend: Backend,
}

impl Store {
    pub fn new(dir: PathBuf, backend: Backend) -> Store {
        Store { dir, backend }
    }

    fn file(&self, user: &str) -> PathBuf {
        self.dir.join(format!("{user}.json"))
    }

    pub fn load(&self, user: &str) -> Vec<Credential> {
        fs::read(self.file(user)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, user: &str, creds: &[Credential]) -> io::Result<()> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir)?;
        let tmp = self.dir.join(format!(".{user}.json.tmp"));
        let data = serde_json::to_vec_pretty(creds)?;
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
            f.write_all(&data)?;
            f.sync_all()?;
        }
        fs::rename(tmp, self.file(user))
    }

    pub fn new_key(&mut self) -> Result<(KeyBlob, PublicKey), String> {
        match &mut self.backend {
            Backend::Tpm(t) => t.create(),
            Backend::Soft => {
                let key = SigningKey::random(&mut rand::rngs::OsRng);
                let point = key.verifying_key().to_encoded_point(false);
                let pk = PublicKey {
                    x: (*point.x().unwrap()).into(),
                    y: (*point.y().unwrap()).into(),
                };
                Ok((KeyBlob::Soft { secret: hex::encode(key.to_bytes()) }, pk))
            }
        }
    }

    /// ECDSA P-256 / SHA-256 signature over msg, DER encoded
    pub fn sign(&mut self, blob: &KeyBlob, msg: &[u8]) -> Result<Vec<u8>, String> {
        match (blob, &mut self.backend) {
            (KeyBlob::Tpm { private, public }, Backend::Tpm(t)) => {
                let digest: [u8; 32] = Sha256::digest(msg).into();
                let (r, s) = t.sign(private, public, &digest)?;
                Ok(der_signature(&r, &s))
            }
            (KeyBlob::Soft { secret }, _) => {
                let bytes = hex::decode(secret).map_err(|e| e.to_string())?;
                let key = SigningKey::from_slice(&bytes).map_err(|e| e.to_string())?;
                let sig: Signature = key.sign(msg);
                Ok(sig.to_der().as_bytes().to_vec())
            }
            _ => Err("credential was created with a different key backend".into()),
        }
    }
}

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut v);
    v
}

/// DER SEQUENCE { INTEGER r, INTEGER s } from big-endian r and s
pub fn der_signature(r: &[u8], s: &[u8]) -> Vec<u8> {
    fn int(v: &[u8]) -> Vec<u8> {
        let mut v: Vec<u8> = v.iter().copied().skip_while(|&b| b == 0).collect();
        if v.is_empty() {
            v.push(0);
        }
        if v[0] & 0x80 != 0 {
            v.insert(0, 0);
        }
        let mut out = vec![0x02, v.len() as u8];
        out.extend(v);
        out
    }
    let body = [int(r), int(s)].concat();
    let mut out = vec![0x30, body.len() as u8];
    out.extend(body);
    out
}

pub mod tpm {
    use super::{KeyBlob, PublicKey};
    use std::str::FromStr;
    use tss_esapi::attributes::ObjectAttributesBuilder;
    use tss_esapi::constants::tss::{TPM2_RH_NULL, TPM2_ST_HASHCHECK};
    use tss_esapi::handles::KeyHandle;
    use tss_esapi::interface_types::algorithm::{HashingAlgorithm, PublicAlgorithm};
    use tss_esapi::interface_types::ecc::EccCurve;
    use tss_esapi::interface_types::resource_handles::Hierarchy;
    use tss_esapi::structures::{
        Digest, EccPoint, EccScheme, HashScheme, HashcheckTicket, Private, Public, PublicBuilder,
        PublicEccParametersBuilder, Signature, SignatureScheme, SymmetricDefinitionObject,
    };
    use tss_esapi::tcti_ldr::{DeviceConfig, TctiNameConf};
    use tss_esapi::traits::{Marshall, UnMarshall};
    use tss_esapi::tss2_esys::TPMT_TK_HASHCHECK;
    use tss_esapi::Context;

    pub struct Tpm {
        ctx: Context,
        primary: KeyHandle,
    }

    fn err<E: std::fmt::Display>(e: E) -> String {
        format!("tpm: {e}")
    }

    impl Tpm {
        pub fn open() -> Result<Tpm, String> {
            let tcti = TctiNameConf::Device(DeviceConfig::from_str("/dev/tpmrm0").map_err(err)?);
            let mut ctx = Context::new(tcti).map_err(err)?;

            // ECC P-256 storage key; deterministic for the owner seed, so it is recreated
            // identically on every start and nothing persistent is written to the TPM.
            let attrs = ObjectAttributesBuilder::new()
                .with_fixed_tpm(true)
                .with_fixed_parent(true)
                .with_sensitive_data_origin(true)
                .with_user_with_auth(true)
                .with_restricted(true)
                .with_decrypt(true)
                .with_no_da(true)
                .build()
                .map_err(err)?;
            let params = PublicEccParametersBuilder::new_restricted_decryption_key(
                SymmetricDefinitionObject::AES_128_CFB,
                EccCurve::NistP256,
            )
            .build()
            .map_err(err)?;
            let template = PublicBuilder::new()
                .with_public_algorithm(PublicAlgorithm::Ecc)
                .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
                .with_object_attributes(attrs)
                .with_ecc_parameters(params)
                .with_ecc_unique_identifier(EccPoint::default())
                .build()
                .map_err(err)?;
            let primary = ctx
                .execute_with_nullauth_session(|c| c.create_primary(Hierarchy::Owner, template, None, None, None, None))
                .map_err(err)?
                .key_handle;
            Ok(Tpm { ctx, primary })
        }

        pub fn create(&mut self) -> Result<(KeyBlob, PublicKey), String> {
            let attrs = ObjectAttributesBuilder::new()
                .with_fixed_tpm(true)
                .with_fixed_parent(true)
                .with_sensitive_data_origin(true)
                .with_user_with_auth(true)
                .with_sign_encrypt(true)
                .with_no_da(true)
                .build()
                .map_err(err)?;
            let params = PublicEccParametersBuilder::new()
                .with_ecc_scheme(EccScheme::EcDsa(HashScheme::new(HashingAlgorithm::Sha256)))
                .with_curve(EccCurve::NistP256)
                .with_is_signing_key(true)
                .with_is_decryption_key(false)
                .with_restricted(false)
                .with_key_derivation_function_scheme(tss_esapi::structures::KeyDerivationFunctionScheme::Null)
                .build()
                .map_err(err)?;
            let template = PublicBuilder::new()
                .with_public_algorithm(PublicAlgorithm::Ecc)
                .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
                .with_object_attributes(attrs)
                .with_ecc_parameters(params)
                .with_ecc_unique_identifier(EccPoint::default())
                .build()
                .map_err(err)?;
            let primary = self.primary;
            let key = self
                .ctx
                .execute_with_nullauth_session(|c| c.create(primary, template, None, None, None, None))
                .map_err(err)?;
            let (x, y) = match &key.out_public {
                Public::Ecc { unique, .. } => (pad32(unique.x().value()), pad32(unique.y().value())),
                _ => return Err("tpm: unexpected key type".into()),
            };
            let blob = KeyBlob::Tpm {
                private: hex::encode(key.out_private.value()),
                public: hex::encode(key.out_public.marshall().map_err(err)?),
            };
            Ok((blob, PublicKey { x, y }))
        }

        pub fn sign(&mut self, private: &str, public: &str, digest: &[u8; 32]) -> Result<(Vec<u8>, Vec<u8>), String> {
            let private = Private::try_from(hex::decode(private).map_err(err)?).map_err(err)?;
            let public = Public::unmarshall(&hex::decode(public).map_err(err)?).map_err(err)?;
            let digest = Digest::try_from(digest.to_vec()).map_err(err)?;
            let ticket = HashcheckTicket::try_from(TPMT_TK_HASHCHECK {
                tag: TPM2_ST_HASHCHECK,
                hierarchy: TPM2_RH_NULL,
                digest: Default::default(),
            })
            .map_err(err)?;
            let primary = self.primary;
            let sig = self
                .ctx
                .execute_with_nullauth_session(|c| {
                    let key = c.load(primary, private, public)?;
                    let sig = c.sign(key, digest, SignatureScheme::Null, ticket);
                    c.flush_context(key.into())?;
                    sig
                })
                .map_err(err)?;
            match sig {
                Signature::EcDsa(s) => Ok((s.signature_r().value().to_vec(), s.signature_s().value().to_vec())),
                _ => Err("tpm: unexpected signature type".into()),
            }
        }
    }

    fn pad32(v: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        let n = v.len().min(32);
        out[32 - n..].copy_from_slice(&v[v.len() - n..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn der_encoding() {
        // high bit set -> leading zero; leading zeros stripped
        let r = [0x80u8; 32];
        let mut s = [0u8; 32];
        s[31] = 1;
        let der = der_signature(&r, &s);
        assert_eq!(der[0], 0x30);
        assert_eq!(&der[2..5], &[0x02, 33, 0x00]);
        assert_eq!(&der[der.len() - 3..], &[0x02, 1, 1]);
        assert!(Signature::from_der(&der).is_ok());
    }
}

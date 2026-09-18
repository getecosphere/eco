//! Publisher attestations for LXS releases.
//!
//! `fetch_lxs_to_cache` already checks that a downloaded binary matches the
//! sha256 recorded in its manifest — that is *integrity*. It says nothing
//! about who produced the manifest, because an attacker who can rewrite the
//! binary can rewrite the hash next to it. This module adds *authenticity*:
//! an Ed25519 signature over a canonical rendering of the release identity
//! (name, version, publisher, per-arch artifact hashes), plus a hash-chained
//! transparency log so a publisher cannot quietly re-sign a different release
//! under the same version.
//!
//! Signing needs an Ed25519 implementation. We reuse `ring`, which the client
//! already links transitively through rustls on macOS/Linux; the dependency is
//! declared only for non-Windows targets because `ring` does not cross-compile
//! to the Windows GNU target the client is otherwise buildable for.

use base64::Engine;
use std::path::PathBuf;

/// Domain-separation prefix. Bump when the signed byte layout changes.
pub const ATTEST_DOMAIN: &str = "lxs-attestation-v1";

/// Canonical bytes a publisher signs for a release. Deterministic: callers
/// pass artifacts in any order and this sorts by arch before rendering.
/// `contract_digest` binds the declared contract (env schema, db, network,
/// resources) into the signature so a registry cannot rewrite grants or
/// egress rules without invalidating it.
pub fn canonical_bytes(
    name: &str,
    version: &str,
    publisher: &str,
    artifacts: &[(String, String)],
    contract_digest: &str,
) -> Vec<u8> {
    let mut rows: Vec<(&String, &String)> = artifacts.iter().map(|(a, s)| (a, s)).collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let mut s = String::new();
    s.push_str(ATTEST_DOMAIN);
    s.push('\n');
    s.push_str(&format!("name={name}\n"));
    s.push_str(&format!("version={version}\n"));
    s.push_str(&format!("publisher={publisher}\n"));
    s.push_str(&format!("contract={contract_digest}\n"));
    for (arch, sha) in rows {
        s.push_str(&format!("artifact {arch} {sha}\n"));
    }
    s.into_bytes()
}

pub fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn unb64(text: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|e| format!("invalid base64: {e}"))
}

/// Where the publisher's private signing key lives (PKCS#8 DER, mode 0600).
pub fn signing_key_path() -> PathBuf {
    PathBuf::from(crate::util::home_dir())
        .join(".eco")
        .join("lxs-signing.key")
}

/// Trusted-publisher directory: `<registry>/keys/<publisher>.pub` (registry
/// owners publish the key alongside the release) and
/// `~/.eco/trusted-keys/<publisher>.pub` (local pin). Either may be absent.
pub fn trusted_pubkey(publisher: &str, registry_root: Option<&std::path::Path>) -> Option<Vec<u8>> {
    let file = format!("{publisher}.pub");
    let mut candidates = Vec::new();
    candidates.push(
        PathBuf::from(crate::util::home_dir())
            .join(".eco")
            .join("trusted-keys")
            .join(&file),
    );
    if let Some(root) = registry_root {
        candidates.push(root.join("keys").join(&file));
    }
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(bytes) = unb64(text.trim()) {
                if bytes.len() == 32 {
                    return Some(bytes);
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};

    pub fn load_or_create_signing_key() -> Result<Vec<u8>, String> {
        let path = signing_key_path();
        if let Ok(bytes) = std::fs::read(&path) {
            return Ok(bytes);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| "failed to generate signing key".to_string())?;
        std::fs::write(&path, pkcs8.as_ref()).map_err(|e| e.to_string())?;
        restrict_0600(&path);
        Ok(pkcs8.as_ref().to_vec())
    }

    pub fn public_key(pkcs8: &[u8]) -> Result<Vec<u8>, String> {
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| "invalid signing key".to_string())?;
        Ok(kp.public_key().as_ref().to_vec())
    }

    pub fn sign(pkcs8: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| "invalid signing key".to_string())?;
        Ok(kp.sign(msg).as_ref().to_vec())
    }

    pub fn verify(pubkey: &[u8], msg: &[u8], sig: &[u8]) -> bool {
        if pubkey.len() != 32 || sig.len() != 64 {
            return false;
        }
        UnparsedPublicKey::new(&ED25519, pubkey)
            .verify(msg, sig)
            .is_ok()
    }

    #[cfg(unix)]
    fn restrict_0600(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }

    #[cfg(not(unix))]
    fn restrict_0600(_path: &std::path::Path) {}
}

#[cfg(target_os = "windows")]
mod imp {
    pub fn load_or_create_signing_key() -> Result<Vec<u8>, String> {
        Err("LXS attestation requires macOS or Linux".to_string())
    }
    pub fn public_key(_pkcs8: &[u8]) -> Result<Vec<u8>, String> {
        Err("LXS attestation requires macOS or Linux".to_string())
    }
    pub fn sign(_pkcs8: &[u8], _msg: &[u8]) -> Result<Vec<u8>, String> {
        Err("LXS attestation requires macOS or Linux".to_string())
    }
    pub fn verify(_pubkey: &[u8], _msg: &[u8], _sig: &[u8]) -> bool {
        false
    }
}

pub use imp::{load_or_create_signing_key, public_key, sign, verify};

/// Append a release to the registry transparency log and return the line's
/// chained hash. The log is a hash chain: each line commits to the previous
/// line, so removing or reordering entries is detectable.
pub fn append_transparency(
    registry_root: &std::path::Path,
    name: &str,
    version: &str,
    publisher: &str,
    manifest_digest: &str,
    signature: &str,
) -> Result<String, String> {
    let log_path = registry_root.join("transparency.log");
    let prev = std::fs::read_to_string(&log_path)
        .ok()
        .and_then(|text| text.lines().last().map(|l| line_hash(l)))
        .unwrap_or_else(|| line_hash("genesis"));
    let ts = crate::commands::lxs::now_rfc3339();
    let body = format!(
        "{ts} {name}@{version} publisher={publisher} digest={manifest_digest} sig={signature}"
    );
    let chained = line_hash(&format!("{prev} {body}"));
    let line = format!("{chained} {body}\n");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("open {}: {e}", log_path.display()))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("append transparency log: {e}"))?;
    Ok(chained)
}

/// SHA-256 hex of a string, used for the transparency hash chain.
pub fn line_hash(text: &str) -> String {
    use sha2::Digest;
    crate::registry::hex_encode(&sha2::Sha256::digest(text.as_bytes()))
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip_and_canonical_order() {
        let key = load_or_create_signing_key().expect("signing key");
        let a = canonical_bytes(
            "auth",
            "3.9.0",
            "stuff8",
            &[
                ("linux/amd64".to_string(), "aa".to_string()),
                ("darwin/arm64".to_string(), "bb".to_string()),
            ],
            "deadbeef",
        );
        let b = canonical_bytes(
            "auth",
            "3.9.0",
            "stuff8",
            &[
                ("darwin/arm64".to_string(), "bb".to_string()),
                ("linux/amd64".to_string(), "aa".to_string()),
            ],
            "deadbeef",
        );
        assert_eq!(a, b, "canonical bytes must not depend on map order");

        let sig = sign(&key, &a).expect("sign");
        let pk = public_key(&key).expect("public key");
        assert!(verify(&pk, &a, &sig), "valid signature must verify");

        let mut tampered = a.clone();
        tampered.push(b'!');
        assert!(!verify(&pk, &tampered, &sig), "tampered message must fail");
        assert!(!verify(&pk, &a, &sig[..sig.len() - 1]), "truncated sig must fail");
    }
}

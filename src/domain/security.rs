use std::io::{Read, Write};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use rand::RngCore;
use rand::rngs::OsRng;

use crate::models::*;

pub(crate) fn create_security_metadata(
    password: &str,
) -> Result<(SecurityMetadata, KeyMaterial), String> {
    let mut salt = [0_u8; 16];
    OsRng.fill_bytes(&mut salt);

    let key = derive_key(password, &salt)?;
    let verifier = encrypt_bytes(&key, VERIFIER_PLAINTEXT)?;

    Ok((
        SecurityMetadata {
            version: 1,
            salt_b64: BASE64.encode(salt),
            verifier,
        },
        key,
    ))
}

pub(crate) fn verify_password(
    password: &str,
    metadata: &SecurityMetadata,
) -> Result<KeyMaterial, String> {
    let salt = BASE64
        .decode(metadata.salt_b64.as_bytes())
        .map_err(|err| format!("invalid metadata salt: {err}"))?;
    let key = derive_key(password, &salt)?;

    let verifier = decrypt_bytes(&key, &metadata.verifier)?;
    if verifier != VERIFIER_PLAINTEXT {
        return Err("password verification failed".to_owned());
    }

    Ok(key)
}

fn derive_key(password: &str, salt: &[u8]) -> Result<KeyMaterial, String> {
    let mut key = [0_u8; 32];
    let params = Params::new(64 * 1024, 3, 1, Some(32)).map_err(|err| err.to_string())?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|err| err.to_string())?;
    Ok(key)
}

pub(crate) fn encrypt_bytes(key: &KeyMaterial, plaintext: &[u8]) -> Result<EncryptedBlob, String> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce_bytes = [0_u8; 24];
    OsRng.fill_bytes(&mut nonce_bytes);

    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|err| format!("encryption failed: {err}"))?;

    Ok(EncryptedBlob {
        version: 1,
        nonce_b64: BASE64.encode(nonce_bytes),
        ciphertext_b64: BASE64.encode(ciphertext),
    })
}

pub(crate) fn decrypt_bytes(key: &KeyMaterial, blob: &EncryptedBlob) -> Result<Vec<u8>, String> {
    let nonce = BASE64
        .decode(blob.nonce_b64.as_bytes())
        .map_err(|err| format!("invalid nonce encoding: {err}"))?;
    if nonce.len() != 24 {
        return Err("invalid nonce length".to_owned());
    }

    let ciphertext = BASE64
        .decode(blob.ciphertext_b64.as_bytes())
        .map_err(|err| format!("invalid ciphertext encoding: {err}"))?;

    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(XNonce::from_slice(&nonce), ciphertext.as_slice())
        .map_err(|err| format!("decryption failed: {err}"))
}

pub(crate) fn serialize_workspace_bundle(
    payload: &SharedWorkspacePayload,
    password: &str,
) -> Result<String, String> {
    if password.trim().is_empty() {
        return Err("Bundle password is required.".to_owned());
    }

    let payload_bytes = serde_json::to_vec(payload)
        .map_err(|err| format!("Failed to encode payload JSON: {err}"))?;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&payload_bytes)
        .map_err(|err| format!("Failed to gzip payload: {err}"))?;
    let compressed = encoder
        .finish()
        .map_err(|err| format!("Failed to finalize gzip payload: {err}"))?;

    let mut salt = [0_u8; 16];
    OsRng.fill_bytes(&mut salt);
    let key = derive_key(password, &salt)?;
    let encrypted = encrypt_bytes(&key, &compressed)?;

    let bundle = SharedWorkspaceBundleFile {
        version: 1,
        salt_b64: BASE64.encode(salt),
        encrypted,
    };

    serde_json::to_string_pretty(&bundle)
        .map_err(|err| format!("Failed to encode bundle JSON: {err}"))
}

pub(crate) fn deserialize_workspace_bundle(
    raw: &str,
    password: &str,
) -> Result<SharedWorkspacePayload, String> {
    if password.trim().is_empty() {
        return Err("Bundle password is required.".to_owned());
    }

    let bundle = serde_json::from_str::<SharedWorkspaceBundleFile>(raw)
        .map_err(|err| format!("Invalid bundle file JSON: {err}"))?;
    if bundle.version != 1 {
        return Err(format!("Unsupported bundle version: {}", bundle.version));
    }

    let salt = BASE64
        .decode(bundle.salt_b64.as_bytes())
        .map_err(|err| format!("Invalid bundle salt: {err}"))?;
    let key = derive_key(password, &salt)?;
    let compressed = decrypt_bytes(&key, &bundle.encrypted)?;

    let mut decoder = GzDecoder::new(compressed.as_slice());
    let mut payload_bytes = Vec::new();
    decoder
        .read_to_end(&mut payload_bytes)
        .map_err(|err| format!("Failed to decompress bundle payload: {err}"))?;

    let payload = serde_json::from_slice::<SharedWorkspacePayload>(&payload_bytes)
        .map_err(|err| format!("Invalid bundle payload JSON: {err}"))?;
    if payload.version != 1 {
        return Err(format!("Unsupported payload version: {}", payload.version));
    }

    Ok(payload)
}

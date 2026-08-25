use std::io::Cursor;

use pgp::{
    composed::{Deserializable, DetachedSignature, SignedPublicKey},
    types::KeyDetails,
};
use torben_contracts::{TorbenError, TorbenResult};

pub(crate) const ADOPTIUM_RELEASE_FINGERPRINT: &str = "3B04D753C9050D9A5D343F39843C48A565F8F04B";

pub(crate) fn verify_archive_signature(
    archive: &[u8],
    signature_bytes: &[u8],
    public_key_bytes: &[u8],
) -> TorbenResult<()> {
    verify_with_fingerprint(
        archive,
        signature_bytes,
        public_key_bytes,
        ADOPTIUM_RELEASE_FINGERPRINT,
    )
}

fn verify_with_fingerprint(
    archive: &[u8],
    signature_bytes: &[u8],
    public_key_bytes: &[u8],
    trusted_fingerprint: &str,
) -> TorbenResult<()> {
    if archive.is_empty() || signature_bytes.is_empty() || public_key_bytes.is_empty() {
        return Err(TorbenError::new(
            "temurin_signature_missing",
            "The Eclipse Temurin archive, signature, or public key is missing.",
        ));
    }
    let (key, _) =
        SignedPublicKey::from_armor_single(Cursor::new(public_key_bytes)).map_err(|error| {
            TorbenError::new(
                "temurin_release_key_invalid",
                "The official Adoptium release key is malformed.",
            )
            .with_detail("reason", error.to_string())
        })?;
    let actual_fingerprint = format!("{:X}", key.fingerprint());
    if actual_fingerprint != trusted_fingerprint {
        return Err(TorbenError::new(
            "temurin_release_key_untrusted",
            "The Adoptium release key fingerprint is not trusted.",
        )
        .with_detail("expected", trusted_fingerprint)
        .with_detail("actual", actual_fingerprint));
    }
    key.verify_bindings().map_err(|error| {
        TorbenError::new(
            "temurin_release_key_invalid",
            "The Adoptium release key bindings are invalid.",
        )
        .with_detail("reason", error.to_string())
    })?;
    let signature = if signature_bytes.starts_with(b"-----BEGIN PGP SIGNATURE-----") {
        DetachedSignature::from_armor_single(Cursor::new(signature_bytes)).map(|(value, _)| value)
    } else {
        DetachedSignature::from_bytes(Cursor::new(signature_bytes))
    }
    .map_err(|error| {
        TorbenError::new(
            "temurin_signature_invalid",
            "The Eclipse Temurin detached signature is malformed.",
        )
        .with_detail("reason", error.to_string())
    })?;

    if issuer_matches(&signature, &key) && signature.verify(&key, archive).is_ok() {
        return Ok(());
    }
    for subkey in &key.public_subkeys {
        if issuer_matches(&signature, subkey) && signature.verify(subkey, archive).is_ok() {
            return Ok(());
        }
    }
    Err(TorbenError::new(
        "temurin_signature_invalid",
        "The Eclipse Temurin archive signature is invalid.",
    ))
}

fn issuer_matches(signature: &DetachedSignature, key: &impl KeyDetails) -> bool {
    let fingerprints = signature.signature.issuer_fingerprint();
    let key_ids = signature.signature.issuer_key_id();
    if fingerprints.is_empty() && key_ids.is_empty() {
        return true;
    }
    fingerprints
        .iter()
        .any(|issuer| **issuer == key.fingerprint())
        || key_ids.iter().any(|issuer| **issuer == key.legacy_key_id())
}

#[cfg(test)]
mod tests {
    use super::{verify_archive_signature, verify_with_fingerprint};

    const NODE_MANIFEST: &[u8] =
        include_bytes!("../assets/node-signature-fixtures/v24.19.0-SHASUMS256.txt");
    const NODE_PUBLIC_KEY: &[u8] =
        include_bytes!("../assets/node-release-keys/5BE8A3F6C8A5C01D106C0AD820B1A390B168D356.asc");
    const NODE_SIGNATURE_HEX: &str = "887504001608001D1621045BE8A3F6C8A5C01D106C0AD820B1A390B168D35605026A709B58000A091020B1A390B168D356914300FF4E7E884D9979816A9982E075022E19D56D91F6BAAC4481A2790E53931438CA730100E97B359FC84D02DC2BFB3A3D5E2B754A5E23DC0EC144E6E187D7E977D597D40B";

    #[test]
    fn rejects_a_malformed_release_key() {
        let error = verify_archive_signature(b"archive", b"signature", b"not a key").unwrap_err();
        assert_eq!(error.code, "temurin_release_key_invalid");
    }

    #[test]
    fn rejects_missing_integrity_inputs() {
        let error = verify_archive_signature(&[], b"signature", b"key").unwrap_err();
        assert_eq!(error.code, "temurin_signature_missing");
    }

    #[test]
    fn verifies_detached_signature_with_a_pinned_primary_fingerprint() {
        verify_with_fingerprint(
            NODE_MANIFEST,
            &hex::decode(NODE_SIGNATURE_HEX).unwrap(),
            NODE_PUBLIC_KEY,
            "5BE8A3F6C8A5C01D106C0AD820B1A390B168D356",
        )
        .unwrap();
    }

    #[test]
    fn rejects_valid_signature_when_the_primary_fingerprint_is_not_pinned() {
        let error = verify_with_fingerprint(
            NODE_MANIFEST,
            &hex::decode(NODE_SIGNATURE_HEX).unwrap(),
            NODE_PUBLIC_KEY,
            "0000000000000000000000000000000000000000",
        )
        .unwrap_err();
        assert_eq!(error.code, "temurin_release_key_untrusted");
    }
}

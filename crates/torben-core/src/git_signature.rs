use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
};

use pgp::composed::{CleartextSignedMessage, Deserializable, DetachedSignature, SignedPublicKey};
use pgp::types::KeyDetails;
use torben_contracts::{TorbenError, TorbenResult};
use xz2::read::XzDecoder;

pub(crate) const KERNEL_CHECKSUM_FINGERPRINT: &str = "B8868C80BA62A1FFFAF5FDA9632D3A06589DA6B1";

const KERNEL_CHECKSUM_KEY: &str = include_str!("../assets/git-kernel-checksum-autosigner.asc");
pub(crate) const GIT_RELEASE_FINGERPRINT: &str = "96E07AF25771955980DAD10020D04E5A713660A7";
const GIT_RELEASE_KEY: &str = include_str!("../assets/git-release-key.asc");
const MAX_UNCOMPRESSED_TAR_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn verify_kernel_checksum_manifest(manifest: &[u8]) -> TorbenResult<String> {
    let text = std::str::from_utf8(manifest).map_err(|error| {
        TorbenError::new(
            "git_checksum_manifest_invalid",
            "The kernel.org Git checksum manifest is not valid UTF-8.",
        )
        .with_detail("reason", error.to_string())
    })?;
    let (key, _) =
        SignedPublicKey::from_armor_single(Cursor::new(KERNEL_CHECKSUM_KEY)).map_err(|error| {
            TorbenError::new(
                "git_checksum_key_invalid",
                "The embedded kernel.org checksum signing key is malformed.",
            )
            .with_detail("reason", error.to_string())
        })?;
    let actual_fingerprint = format!("{:X}", key.fingerprint());
    if actual_fingerprint != KERNEL_CHECKSUM_FINGERPRINT {
        return Err(TorbenError::new(
            "git_checksum_key_untrusted",
            "The embedded kernel.org checksum signing key is not trusted.",
        )
        .with_detail("expected", KERNEL_CHECKSUM_FINGERPRINT)
        .with_detail("actual", actual_fingerprint));
    }
    key.verify_bindings().map_err(|error| {
        TorbenError::new(
            "git_checksum_key_invalid",
            "The embedded kernel.org checksum key bindings are invalid.",
        )
        .with_detail("reason", error.to_string())
    })?;
    let (message, _) = CleartextSignedMessage::from_string(text).map_err(|error| {
        TorbenError::new(
            "git_checksum_signature_invalid",
            "The kernel.org Git checksum manifest is not a valid clear-signed message.",
        )
        .with_detail("reason", error.to_string())
    })?;
    let verified = message.verify(&key).is_ok()
        || key
            .public_subkeys
            .iter()
            .any(|subkey| message.verify(subkey).is_ok());
    if !verified {
        return Err(TorbenError::new(
            "git_checksum_signature_invalid",
            "The kernel.org Git checksum manifest signature is invalid.",
        ));
    }
    Ok(message.signed_text())
}

pub(crate) fn verify_git_release_archive(
    archive_path: &Path,
    signature_bytes: &[u8],
) -> TorbenResult<()> {
    let key = parse_pinned_key(
        GIT_RELEASE_KEY.as_bytes(),
        GIT_RELEASE_FINGERPRINT,
        "Git release",
    )?;
    let signature = parse_detached_signature(signature_bytes)?;
    let file = File::open(archive_path).map_err(|error| {
        TorbenError::new(
            "git_release_archive_unavailable",
            "The verified Git source archive could not be opened for signature checking.",
        )
        .with_detail("path", archive_path.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    let mut uncompressed = Vec::new();
    XzDecoder::new(file)
        .take(MAX_UNCOMPRESSED_TAR_BYTES + 1)
        .read_to_end(&mut uncompressed)
        .map_err(|error| {
            TorbenError::new(
                "git_release_archive_invalid",
                "The Git source archive could not be decompressed for signature checking.",
            )
            .with_detail("reason", error.to_string())
        })?;
    if u64::try_from(uncompressed.len()).unwrap_or(u64::MAX) > MAX_UNCOMPRESSED_TAR_BYTES {
        return Err(TorbenError::new(
            "git_release_archive_too_large",
            "The uncompressed Git source archive exceeds the verification limit.",
        ));
    }
    verify_with_key(&uncompressed, &signature, &key)
}

fn parse_pinned_key(
    public_key_bytes: &[u8],
    trusted_fingerprint: &str,
    label: &str,
) -> TorbenResult<SignedPublicKey> {
    let (key, _) =
        SignedPublicKey::from_armor_single(Cursor::new(public_key_bytes)).map_err(|error| {
            TorbenError::new(
                "git_release_key_invalid",
                "The embedded Git release signing key is malformed.",
            )
            .with_detail("key", label)
            .with_detail("reason", error.to_string())
        })?;
    let actual = format!("{:X}", key.fingerprint());
    if actual != trusted_fingerprint {
        return Err(TorbenError::new(
            "git_release_key_untrusted",
            "The embedded Git release signing key is not trusted.",
        )
        .with_detail("expected", trusted_fingerprint)
        .with_detail("actual", actual));
    }
    key.verify_bindings().map_err(|error| {
        TorbenError::new(
            "git_release_key_invalid",
            "The embedded Git release key bindings are invalid.",
        )
        .with_detail("reason", error.to_string())
    })?;
    Ok(key)
}

fn parse_detached_signature(signature_bytes: &[u8]) -> TorbenResult<DetachedSignature> {
    if signature_bytes.is_empty() {
        return Err(TorbenError::new(
            "git_release_signature_missing",
            "The official Git source signature is missing.",
        ));
    }
    let parsed = if signature_bytes.starts_with(b"-----BEGIN PGP SIGNATURE-----") {
        DetachedSignature::from_armor_single(Cursor::new(signature_bytes)).map(|(value, _)| value)
    } else {
        DetachedSignature::from_bytes(Cursor::new(signature_bytes))
    };
    parsed.map_err(|error| {
        TorbenError::new(
            "git_release_signature_invalid",
            "The official Git source signature is malformed.",
        )
        .with_detail("reason", error.to_string())
    })
}

fn verify_with_key(
    archive: &[u8],
    signature: &DetachedSignature,
    key: &SignedPublicKey,
) -> TorbenResult<()> {
    if signature.verify(key, archive).is_ok()
        || key
            .public_subkeys
            .iter()
            .any(|subkey| signature.verify(subkey, archive).is_ok())
    {
        Ok(())
    } else {
        Err(TorbenError::new(
            "git_release_signature_invalid",
            "The Git source archive was not signed by the pinned upstream release key.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};
    use pgp::types::KeyDetails;

    use super::{
        GIT_RELEASE_FINGERPRINT, GIT_RELEASE_KEY, KERNEL_CHECKSUM_FINGERPRINT, KERNEL_CHECKSUM_KEY,
        parse_detached_signature, parse_pinned_key, verify_kernel_checksum_manifest,
        verify_with_key,
    };

    #[test]
    fn embedded_kernel_checksum_key_matches_the_pinned_fingerprint() {
        let (key, _) =
            SignedPublicKey::from_armor_single(Cursor::new(KERNEL_CHECKSUM_KEY)).unwrap();
        assert_eq!(
            format!("{:X}", key.fingerprint()),
            KERNEL_CHECKSUM_FINGERPRINT
        );
        key.verify_bindings().unwrap();
    }

    #[test]
    fn malformed_kernel_checksum_manifest_fails_closed() {
        let error = verify_kernel_checksum_manifest(b"not a signed manifest").unwrap_err();
        assert_eq!(error.code, "git_checksum_signature_invalid");
    }

    #[test]
    fn embedded_git_release_key_matches_the_pinned_primary_fingerprint() {
        let key = parse_pinned_key(
            GIT_RELEASE_KEY.as_bytes(),
            GIT_RELEASE_FINGERPRINT,
            "Git release",
        )
        .unwrap();
        assert_eq!(format!("{:X}", key.fingerprint()), GIT_RELEASE_FINGERPRINT);
        assert!(key.public_subkeys.iter().any(|subkey| {
            format!("{:X}", subkey.fingerprint()) == "E1F036B1FEE7221FC778ECEFB0B5E88696AFE6CB"
        }));
    }

    #[test]
    fn detached_release_verification_accepts_a_bound_signing_key() {
        const MANIFEST: &[u8] =
            include_bytes!("../assets/node-signature-fixtures/v24.19.0-SHASUMS256.txt");
        const PUBLIC_KEY: &[u8] = include_bytes!(
            "../assets/node-release-keys/5BE8A3F6C8A5C01D106C0AD820B1A390B168D356.asc"
        );
        const SIGNATURE_HEX: &str = "887504001608001D1621045BE8A3F6C8A5C01D106C0AD820B1A390B168D35605026A709B58000A091020B1A390B168D356914300FF4E7E884D9979816A9982E075022E19D56D91F6BAAC4481A2790E53931438CA730100E97B359FC84D02DC2BFB3A3D5E2B754A5E23DC0EC144E6E187D7E977D597D40B";
        let key = parse_pinned_key(
            PUBLIC_KEY,
            "5BE8A3F6C8A5C01D106C0AD820B1A390B168D356",
            "fixture",
        )
        .unwrap();
        let signature =
            DetachedSignature::from_bytes(Cursor::new(hex::decode(SIGNATURE_HEX).unwrap()))
                .unwrap();

        verify_with_key(MANIFEST, &signature, &key).unwrap();
    }

    #[test]
    fn malformed_git_release_signature_fails_closed() {
        let error = parse_detached_signature(b"not a signature").unwrap_err();
        assert_eq!(error.code, "git_release_signature_invalid");
    }

    #[test]
    #[ignore = "requires an explicitly downloaded official kernel.org checksum manifest"]
    fn verifies_the_official_kernel_checksum_manifest() {
        let path = std::env::var_os("TORBEN_GIT_CHECKSUM_FIXTURE")
            .expect("set TORBEN_GIT_CHECKSUM_FIXTURE to the official sha256sums.asc path");
        let manifest = std::fs::read(path).unwrap();
        let signed_text = verify_kernel_checksum_manifest(&manifest).unwrap();
        assert!(signed_text.contains("  git-2.55.0.tar.xz"));
    }
}

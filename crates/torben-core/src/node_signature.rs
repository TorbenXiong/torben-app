use std::io::Cursor;

use pgp::{
    composed::{Deserializable, DetachedSignature, SignedPublicKey},
    types::KeyDetails,
};
use torben_contracts::{TorbenError, TorbenResult};

#[derive(Clone, Copy)]
struct TrustedRoot {
    fingerprint: &'static str,
    armored_key: &'static str,
}

const TRUSTED_ROOTS: &[TrustedRoot] = &[
    TrustedRoot {
        fingerprint: "5BE8A3F6C8A5C01D106C0AD820B1A390B168D356",
        armored_key: include_str!(
            "../assets/node-release-keys/5BE8A3F6C8A5C01D106C0AD820B1A390B168D356.asc"
        ),
    },
    TrustedRoot {
        fingerprint: "DD792F5973C6DE52C432CBDAC77ABFA00DDBF2B7",
        armored_key: include_str!(
            "../assets/node-release-keys/DD792F5973C6DE52C432CBDAC77ABFA00DDBF2B7.asc"
        ),
    },
    TrustedRoot {
        fingerprint: "CC68F5A3106FF448322E48ED27F5E38D5B0A215F",
        armored_key: include_str!(
            "../assets/node-release-keys/CC68F5A3106FF448322E48ED27F5E38D5B0A215F.asc"
        ),
    },
    TrustedRoot {
        fingerprint: "8FCCA13FEF1D0C2E91008E09770F7A9A5AE15600",
        armored_key: include_str!(
            "../assets/node-release-keys/8FCCA13FEF1D0C2E91008E09770F7A9A5AE15600.asc"
        ),
    },
    TrustedRoot {
        fingerprint: "890C08DB8579162FEE0DF9DB8BEAB4DFCF555EF4",
        armored_key: include_str!(
            "../assets/node-release-keys/890C08DB8579162FEE0DF9DB8BEAB4DFCF555EF4.asc"
        ),
    },
    TrustedRoot {
        fingerprint: "C82FA3AE1CBEDC6BE46B9360C43CEC45C17AB93C",
        armored_key: include_str!(
            "../assets/node-release-keys/C82FA3AE1CBEDC6BE46B9360C43CEC45C17AB93C.asc"
        ),
    },
    TrustedRoot {
        fingerprint: "108F52B48DB57BB0CC439B2997B01419BD92F80A",
        armored_key: include_str!(
            "../assets/node-release-keys/108F52B48DB57BB0CC439B2997B01419BD92F80A.asc"
        ),
    },
    TrustedRoot {
        fingerprint: "655F3B5C1FB3FA8D1A0CA6BDE4A7D232B936D2FD",
        armored_key: include_str!(
            "../assets/node-release-keys/655F3B5C1FB3FA8D1A0CA6BDE4A7D232B936D2FD.asc"
        ),
    },
    TrustedRoot {
        fingerprint: "A363A499291CBBC940DD62E41F10027AF002F8B0",
        armored_key: include_str!(
            "../assets/node-release-keys/A363A499291CBBC940DD62E41F10027AF002F8B0.asc"
        ),
    },
];

pub(crate) fn verify_checksum_signature(
    manifest: &[u8],
    signature_bytes: &[u8],
) -> TorbenResult<String> {
    verify_with_roots(manifest, signature_bytes, TRUSTED_ROOTS)
}

fn verify_with_roots(
    manifest: &[u8],
    signature_bytes: &[u8],
    roots: &[TrustedRoot],
) -> TorbenResult<String> {
    if signature_bytes.is_empty() {
        return Err(TorbenError::new(
            "checksum_signature_missing",
            "The official Node.js checksum signature is missing.",
        ));
    }

    let signature = parse_signature(signature_bytes)?;
    let trusted_keys = parse_trusted_roots(roots)?;
    let mut trusted_signer_found = false;

    for (root, key) in roots.iter().zip(&trusted_keys) {
        if issuer_matches(&signature, key) {
            trusted_signer_found = true;
            if signature.verify(key, manifest).is_ok() {
                return Ok(root.fingerprint.to_owned());
            }
        }

        for subkey in &key.public_subkeys {
            if issuer_matches(&signature, subkey) {
                trusted_signer_found = true;
                if signature.verify(subkey, manifest).is_ok() {
                    return Ok(root.fingerprint.to_owned());
                }
            }
        }
    }

    if trusted_signer_found {
        Err(TorbenError::new(
            "checksum_signature_invalid",
            "The official Node.js checksum signature is invalid.",
        ))
    } else {
        Err(TorbenError::new(
            "checksum_signer_untrusted",
            "The Node.js checksum manifest was not signed by a trusted release key.",
        ))
    }
}

fn parse_signature(signature_bytes: &[u8]) -> TorbenResult<DetachedSignature> {
    let parsed = if signature_bytes.starts_with(b"-----BEGIN PGP SIGNATURE-----") {
        DetachedSignature::from_armor_single(Cursor::new(signature_bytes)).map(|(value, _)| value)
    } else {
        DetachedSignature::from_bytes(Cursor::new(signature_bytes))
    };
    parsed.map_err(|error| {
        TorbenError::new(
            "checksum_signature_invalid",
            "The official Node.js checksum signature is malformed.",
        )
        .with_detail("reason", error.to_string())
    })
}

fn parse_trusted_roots(roots: &[TrustedRoot]) -> TorbenResult<Vec<SignedPublicKey>> {
    roots
        .iter()
        .map(|root| {
            let (key, _) = SignedPublicKey::from_armor_single(Cursor::new(root.armored_key))
                .map_err(|error| release_key_error(root.fingerprint, error.to_string()))?;
            let actual = format!("{:X}", key.fingerprint());
            if actual != root.fingerprint {
                return Err(release_key_error(
                    root.fingerprint,
                    format!("embedded key fingerprint is {actual}"),
                ));
            }
            key.verify_bindings()
                .map_err(|error| release_key_error(root.fingerprint, error.to_string()))?;
            Ok(key)
        })
        .collect()
}

fn issuer_matches(signature: &DetachedSignature, key: &impl KeyDetails) -> bool {
    let issuer_fingerprints = signature.signature.issuer_fingerprint();
    let issuer_key_ids = signature.signature.issuer_key_id();
    if issuer_fingerprints.is_empty() && issuer_key_ids.is_empty() {
        return true;
    }
    issuer_fingerprints
        .iter()
        .any(|issuer| **issuer == key.fingerprint())
        || issuer_key_ids
            .iter()
            .any(|issuer| **issuer == key.legacy_key_id())
}

fn release_key_error(fingerprint: &str, reason: String) -> TorbenError {
    TorbenError::new(
        "release_key_invalid",
        "An embedded Node.js release key is invalid.",
    )
    .with_detail("fingerprint", fingerprint)
    .with_detail("reason", reason)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use pgp::composed::{ArmorOptions, Deserializable, DetachedSignature};

    use super::{TRUSTED_ROOTS, parse_trusted_roots, verify_checksum_signature, verify_with_roots};

    const MANIFEST: &[u8] =
        include_bytes!("../assets/node-signature-fixtures/v24.19.0-SHASUMS256.txt");
    const SIGNATURE_HEX: &str = "887504001608001D1621045BE8A3F6C8A5C01D106C0AD820B1A390B168D35605026A709B58000A091020B1A390B168D356914300FF4E7E884D9979816A9982E075022E19D56D91F6BAAC4481A2790E53931438CA730100E97B359FC84D02DC2BFB3A3D5E2B754A5E23DC0EC144E6E187D7E977D597D40B";

    fn signature() -> Vec<u8> {
        hex::decode(SIGNATURE_HEX).unwrap()
    }

    #[test]
    fn embedded_release_keys_match_their_allowlisted_fingerprints() {
        let keys = parse_trusted_roots(TRUSTED_ROOTS).unwrap();
        assert_eq!(keys.len(), TRUSTED_ROOTS.len());
    }

    #[test]
    fn verifies_an_official_detached_signature() {
        assert_eq!(
            verify_checksum_signature(MANIFEST, &signature()).unwrap(),
            "5BE8A3F6C8A5C01D106C0AD820B1A390B168D356"
        );
    }

    #[test]
    fn verifies_an_ascii_armored_detached_signature() {
        let parsed = DetachedSignature::from_bytes(Cursor::new(signature())).unwrap();
        let armored = parsed.to_armored_string(ArmorOptions::default()).unwrap();
        assert!(verify_checksum_signature(MANIFEST, armored.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_a_modified_manifest() {
        let mut manifest = MANIFEST.to_vec();
        manifest[0] ^= 1;
        let error = verify_checksum_signature(&manifest, &signature()).unwrap_err();
        assert_eq!(error.code, "checksum_signature_invalid");
    }

    #[test]
    fn rejects_a_modified_signature() {
        let mut signature = signature();
        let last = signature.last_mut().unwrap();
        *last ^= 1;
        let error = verify_checksum_signature(MANIFEST, &signature).unwrap_err();
        assert_eq!(error.code, "checksum_signature_invalid");
    }

    #[test]
    fn rejects_a_signature_from_a_key_outside_the_selected_trust_roots() {
        let error = verify_with_roots(MANIFEST, &signature(), &TRUSTED_ROOTS[1..]).unwrap_err();
        assert_eq!(error.code, "checksum_signer_untrusted");
    }
}

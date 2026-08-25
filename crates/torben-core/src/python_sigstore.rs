use sigstore_verify::{VerificationPolicy, verify};
use sigstore_verify::{
    trust_root::{SIGSTORE_PRODUCTION_TRUSTED_ROOT, TrustedRoot},
    types::{Bundle, Sha256Hash},
};
use torben_contracts::{TorbenError, TorbenResult};

use crate::python::PythonSigstoreVerifier;

pub(crate) struct ProductionPythonSigstoreVerifier {
    trusted_root: TrustedRoot,
}

impl ProductionPythonSigstoreVerifier {
    pub(crate) fn new() -> TorbenResult<Self> {
        let trusted_root =
            TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT).map_err(|error| {
                TorbenError::new(
                    "python_sigstore_trust_root_invalid",
                    "The embedded Sigstore production trust root is invalid.",
                )
                .with_detail("reason", error.to_string())
            })?;
        Ok(Self { trusted_root })
    }
}

impl PythonSigstoreVerifier for ProductionPythonSigstoreVerifier {
    fn verify(
        &self,
        sha256: &str,
        bundle: &[u8],
        certificate_identity: &str,
        oidc_issuer: &str,
    ) -> TorbenResult<()> {
        let digest = Sha256Hash::from_hex(sha256).map_err(|error| {
            TorbenError::new(
                "python_sigstore_digest_invalid",
                "The CPython artifact digest is invalid.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let bundle = std::str::from_utf8(bundle).map_err(|error| {
            TorbenError::new(
                "python_sigstore_bundle_invalid",
                "The CPython Sigstore bundle is not valid UTF-8.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let bundle = Bundle::from_json(bundle).map_err(|error| {
            TorbenError::new(
                "python_sigstore_bundle_invalid",
                "The CPython Sigstore bundle is malformed.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let policy = VerificationPolicy::default()
            .require_identity(certificate_identity)
            .require_issuer(oidc_issuer);
        verify(digest, &bundle, &policy, &self.trusted_root).map_err(|error| {
            TorbenError::new(
                "python_sigstore_verification_failed",
                "The CPython Sigstore bundle did not verify against the pinned release identity.",
            )
            .with_detail("identity", certificate_identity)
            .with_detail("issuer", oidc_issuer)
            .with_detail("reason", error.to_string())
        })?;
        Ok(())
    }
}

pub(crate) fn verify_production_sigstore_bundle(
    sha256: &str,
    bundle: &Bundle,
    certificate_identity: &str,
    oidc_issuer: &str,
) -> TorbenResult<()> {
    let digest = Sha256Hash::from_hex(sha256).map_err(|error| {
        TorbenError::new("sigstore_digest_invalid", "The artifact digest is invalid.")
            .with_detail("reason", error.to_string())
    })?;
    let trusted_root =
        TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT).map_err(|error| {
            TorbenError::new(
                "sigstore_trust_root_invalid",
                "The embedded Sigstore production trust root is invalid.",
            )
            .with_detail("reason", error.to_string())
        })?;
    let policy = VerificationPolicy::default()
        .require_identity(certificate_identity)
        .require_issuer(oidc_issuer);
    verify(digest, bundle, &policy, &trusted_root).map_err(|error| {
        TorbenError::new(
            "sigstore_verification_failed",
            "The Sigstore bundle did not verify against the pinned release identity.",
        )
        .with_detail("identity", certificate_identity)
        .with_detail("issuer", oidc_issuer)
        .with_detail("reason", error.to_string())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::python::PythonSigstoreVerifier;

    use super::ProductionPythonSigstoreVerifier;

    #[test]
    fn embedded_production_root_parses_and_malformed_bundle_fails_closed() {
        let verifier = ProductionPythonSigstoreVerifier::new().unwrap();
        let error = verifier
            .verify(
                &"00".repeat(32),
                b"{}",
                "hugo@python.org",
                "https://github.com/login/oauth",
            )
            .unwrap_err();
        assert_eq!(error.code, "python_sigstore_bundle_invalid");
    }

    #[test]
    #[ignore = "requires an explicitly downloaded official Python Sigstore bundle"]
    fn verifies_official_python_3147_source_bundle_by_digest() {
        let path = std::env::var_os("TORBEN_PYTHON_SIGSTORE_FIXTURE")
            .expect("set TORBEN_PYTHON_SIGSTORE_FIXTURE to the official bundle path");
        let bundle = std::fs::read(path).unwrap();
        ProductionPythonSigstoreVerifier::new()
            .unwrap()
            .verify(
                "3b48dac8fb59f62eaa67ac83c1eb12bda1b7a08406dd286e252c11a66be27f81",
                &bundle,
                "hugo@python.org",
                "https://github.com/login/oauth",
            )
            .unwrap();
    }
}

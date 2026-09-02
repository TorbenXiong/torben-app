use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime},
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use torben_contracts::{
    AppId, ExactVersion, InstallRecord, InstallScope, OperationState, SourceId, TorbenError,
    TorbenResult, VersionDescriptor,
    plugin::{InstallPlan, InstallStep},
};
use url::Url;

#[cfg(any(unix, test))]
use sigstore_verify::types::{
    Bundle, CanonicalizedBody, DerCertificate, KindVersion, LogId, LogIndex, LogKeyId, MediaType,
    Sha256Hash, SignatureBytes, SignedTimestamp, TransparencyLogEntry, VerificationMaterial,
    bundle::{TimestampVerificationData, VerificationMaterialContent, X509Certificate},
};

use crate::{
    TorbenPaths,
    node::{ArchiveKind, extract_archive_contents, sha256_file_checked},
    operation::{CancellationProbe, OperationJournal},
    process,
};

const RELEASES_URL: &str = "https://api.github.com/repos/openai/codex/releases/";
const MAX_RELEASE_PAGES: usize = 3;
const RELEASES_PER_PAGE: usize = 20;
const MAX_VERSIONS: usize = 5;
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 384 * 1024 * 1024;
const MAX_BUNDLE_BYTES: u64 = 64 * 1024;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(8);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const SIGSTORE_ISSUER: &str = "https://token.actions.githubusercontent.com";

trait CodexSigstoreVerifier: Send + Sync {
    fn verify(
        &self,
        sha256: &str,
        bundle: &[u8],
        certificate_identity: &str,
        oidc_issuer: &str,
    ) -> TorbenResult<()>;
}

#[derive(Debug)]
struct ProductionCodexSigstoreVerifier;

impl CodexSigstoreVerifier for ProductionCodexSigstoreVerifier {
    fn verify(
        &self,
        sha256: &str,
        bundle: &[u8],
        certificate_identity: &str,
        oidc_issuer: &str,
    ) -> TorbenResult<()> {
        #[cfg(unix)]
        {
            let bundle = parse_legacy_cosign_bundle(sha256, bundle)?;
            crate::python_sigstore::verify_production_sigstore_bundle(
                sha256,
                &bundle,
                certificate_identity,
                oidc_issuer,
            )
            .map_err(codex_sigstore_error)
        }
        #[cfg(not(unix))]
        {
            let _ = (sha256, bundle, certificate_identity, oidc_issuer);
            Err(TorbenError::new(
                "codex_sigstore_verifier_unavailable",
                "This Torben App build has no Codex Sigstore verifier.",
            ))
        }
    }
}

#[cfg(any(unix, test))]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyCosignBundle {
    base64_signature: String,
    cert: String,
    rekor_bundle: LegacyRekorBundle,
}

#[cfg(any(unix, test))]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRekorBundle {
    #[serde(rename = "SignedEntryTimestamp")]
    signed_entry_timestamp: String,
    #[serde(rename = "Payload")]
    payload: LegacyRekorPayload,
}

#[cfg(any(unix, test))]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyRekorPayload {
    body: String,
    integrated_time: i64,
    log_index: i64,
    #[serde(rename = "logID")]
    log_id: String,
}

#[cfg(any(unix, test))]
fn parse_legacy_cosign_bundle(sha256: &str, bundle: &[u8]) -> TorbenResult<Bundle> {
    let legacy: LegacyCosignBundle = serde_json::from_slice(bundle).map_err(|error| {
        TorbenError::new(
            "codex_sigstore_bundle_invalid",
            "The Codex legacy Sigstore bundle is malformed.",
        )
        .with_detail("reason", error.to_string())
    })?;
    let pem_bytes = sigstore_verify::types::PayloadBytes::from_base64(&legacy.cert)
        .map_err(codex_sigstore_parse_error)?;
    let pem = std::str::from_utf8(pem_bytes.as_bytes()).map_err(codex_sigstore_parse_error)?;
    let certificate = DerCertificate::from_pem(pem).map_err(codex_sigstore_parse_error)?;
    let signature = SignatureBytes::from_base64(&legacy.base64_signature)
        .map_err(codex_sigstore_parse_error)?;
    let digest = Sha256Hash::from_hex(sha256).map_err(codex_sigstore_parse_error)?;
    let body = CanonicalizedBody::from_base64(&legacy.rekor_bundle.payload.body)
        .map_err(codex_sigstore_parse_error)?;
    let body_json: serde_json::Value =
        serde_json::from_slice(body.as_bytes()).map_err(codex_sigstore_parse_error)?;
    if body_json.get("kind").and_then(serde_json::Value::as_str) != Some("hashedrekord")
        || body_json
            .get("apiVersion")
            .and_then(serde_json::Value::as_str)
            != Some("0.0.1")
        || !is_hex(&legacy.rekor_bundle.payload.log_id, 64)
        || legacy.rekor_bundle.payload.integrated_time <= 0
        || legacy.rekor_bundle.payload.log_index < 0
    {
        return Err(metadata_invalid("legacy Sigstore Rekor entry"));
    }
    let log_id =
        hex::decode(&legacy.rekor_bundle.payload.log_id).map_err(codex_sigstore_parse_error)?;
    let signed_entry_timestamp =
        SignedTimestamp::from_base64(&legacy.rekor_bundle.signed_entry_timestamp)
            .map_err(codex_sigstore_parse_error)?;
    let entry = TransparencyLogEntry {
        log_index: LogIndex::from(legacy.rekor_bundle.payload.log_index),
        log_id: LogId {
            key_id: LogKeyId::from_bytes(&log_id),
        },
        kind_version: KindVersion {
            kind: "hashedrekord".to_owned(),
            version: "0.0.1".to_owned(),
        },
        integrated_time: legacy.rekor_bundle.payload.integrated_time,
        inclusion_promise: Some(sigstore_verify::types::InclusionPromise {
            signed_entry_timestamp,
        }),
        inclusion_proof: None,
        canonicalized_body: body,
    };
    Ok(Bundle {
        media_type: MediaType::Bundle0_1.as_str().to_owned(),
        verification_material: VerificationMaterial {
            content: VerificationMaterialContent::X509CertificateChain {
                certificates: vec![X509Certificate {
                    raw_bytes: certificate,
                }],
            },
            tlog_entries: vec![entry],
            timestamp_verification_data: TimestampVerificationData::default(),
        },
        content: sigstore_verify::types::SignatureContent::MessageSignature(
            sigstore_verify::types::MessageSignature {
                message_digest: Some(sigstore_verify::types::MessageDigest {
                    algorithm: sigstore_verify::types::HashAlgorithm::Sha2256,
                    digest: digest.into(),
                }),
                signature,
            },
        ),
    })
}

#[cfg(any(unix, test))]
fn codex_sigstore_parse_error(error: impl std::fmt::Display) -> TorbenError {
    TorbenError::new(
        "codex_sigstore_bundle_invalid",
        "The Codex legacy Sigstore bundle contains invalid verification material.",
    )
    .with_detail("reason", error.to_string())
}

#[cfg(unix)]
fn codex_sigstore_error(error: TorbenError) -> TorbenError {
    TorbenError::new(
        "codex_sigstore_verification_failed",
        "The Codex release did not verify against the pinned OpenAI workflow identity.",
    )
    .with_detail("reasonCode", error.code)
    .with_detail("reason", error.message)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSigstoreBundle {
    pub url: Url,
    pub checksum: String,
    pub size: u64,
    pub identity: String,
    pub issuer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexDistribution {
    pub version: ExactVersion,
    pub released_at: String,
    pub tag: String,
    pub target: String,
    pub archive_name: String,
    pub archive_url: Url,
    pub archive_checksum: String,
    pub archive_size: u64,
    pub archive_kind: ArchiveKind,
    pub binary_name: String,
    pub sigstore: Option<CodexSigstoreBundle>,
}

#[derive(Clone)]
pub struct CodexProvider {
    client: reqwest::Client,
    releases_url: Url,
    sigstore_verifier: Arc<dyn CodexSigstoreVerifier>,
    fixture_mode: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct GitHubRelease {
    tag_name: String,
    name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    published_at: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct GitHubAsset {
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
    browser_download_url: String,
}

#[derive(Debug, Clone)]
struct TargetAsset {
    target: &'static str,
    archive_name: String,
    binary_name: String,
    archive_kind: ArchiveKind,
    sigstore_name: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum RequestKind {
    Api,
    Asset,
}

impl CodexProvider {
    pub fn official() -> TorbenResult<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
                .https_only(true)
                .build()
                .map_err(network_error)?,
            releases_url: Url::parse(RELEASES_URL).map_err(url_error)?,
            sigstore_verifier: Arc::new(ProductionCodexSigstoreVerifier),
            fixture_mode: false,
        })
    }

    #[cfg(test)]
    fn with_fixture(
        releases_url: Url,
        sigstore_verifier: Arc<dyn CodexSigstoreVerifier>,
    ) -> TorbenResult<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("Torben-App-Test")
                .build()
                .map_err(network_error)?,
            releases_url,
            sigstore_verifier,
            fixture_mode: true,
        })
    }

    pub async fn list_versions(&self) -> TorbenResult<Vec<VersionDescriptor>> {
        let target = current_target_asset()?;
        let mut result = Vec::new();
        let mut needs_more = true;
        let (first_page, second_page) = tokio::join!(self.release_page(1), self.release_page(2));
        for page in [first_page, second_page] {
            needs_more = self.append_release_page(&mut result, page?, &target)?;
            if !needs_more {
                break;
            }
        }
        if needs_more {
            let page = self.release_page(MAX_RELEASE_PAGES).await?;
            let _ = self.append_release_page(&mut result, page, &target)?;
        }
        result.sort_by(|left, right| right.version.cmp(&left.version));
        result.dedup_by(|left, right| left.version == right.version);
        if result.is_empty() {
            return Err(TorbenError::new(
                "codex_catalog_empty",
                "The official Codex release catalog contains no supported stable releases.",
            ));
        }
        if let Some(first) = result.first_mut() {
            first.recommended = true;
        }
        Ok(result)
    }

    fn append_release_page(
        &self,
        result: &mut Vec<VersionDescriptor>,
        releases: Vec<GitHubRelease>,
        target: &TargetAsset,
    ) -> TorbenResult<bool> {
        let count = releases.len();
        for release in releases {
            let Ok(version) = validate_release(&release) else {
                continue;
            };
            match distribution_from(&release, &version, target, self.fixture_mode) {
                Ok(_) => result.push(VersionDescriptor {
                    version,
                    lts_name: None,
                    released_at: release.published_at,
                    recommended: result.is_empty(),
                }),
                Err(error) if error.code == "codex_archive_missing" => continue,
                Err(error) => return Err(error),
            }
            if result.len() == MAX_VERSIONS {
                break;
            }
        }
        Ok(result.len() < MAX_VERSIONS && count == RELEASES_PER_PAGE)
    }

    async fn release_page(&self, page: usize) -> TorbenResult<Vec<GitHubRelease>> {
        let mut url = self.releases_url.clone();
        let path = url.path().trim_end_matches('/').to_owned();
        url.set_path(&path);
        url.query_pairs_mut()
            .append_pair("per_page", &RELEASES_PER_PAGE.to_string())
            .append_pair("page", &page.to_string());
        self.fetch_json(&url).await
    }

    pub async fn resolve_version(&self, requested: &str) -> TorbenResult<ExactVersion> {
        if let Ok(version) = ExactVersion::from_str(requested) {
            self.distribution(&version).await?;
            return Ok(version);
        }
        if matches!(
            requested.trim().to_ascii_lowercase().as_str(),
            "current" | "latest"
        ) {
            return self
                .list_versions()
                .await?
                .into_iter()
                .next()
                .map(|item| item.version)
                .ok_or_else(|| version_not_found(requested));
        }
        Err(TorbenError::new(
            "version_alias_not_found",
            "Use an exact Codex version, 'current', or 'latest'.",
        )
        .with_detail("requested", requested))
    }

    pub async fn distribution(&self, version: &ExactVersion) -> TorbenResult<CodexDistribution> {
        let target = current_target_asset()?;
        let release = self.release(version).await?;
        distribution_from(&release, version, &target, self.fixture_mode)
    }

    #[allow(clippy::too_many_lines)]
    pub async fn install(
        &self,
        paths: &TorbenPaths,
        app_id: &AppId,
        version: &ExactVersion,
        plan: &InstallPlan,
        journal: &mut OperationJournal,
    ) -> TorbenResult<InstallRecord> {
        let distribution = self.validate_install_plan(plan, app_id, version).await?;
        let cancellation = journal.cancellation_probe();
        let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
        if final_path.exists() {
            return Err(TorbenError::new(
                "install_path_exists",
                "The final Codex installation directory already exists.",
            ));
        }
        let download_dir = paths.download_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&download_dir).map_err(io_error)?;
        let archive_path = download_dir.join(&distribution.archive_name);
        let cached = archive_path.is_file()
            && archive_path
                .metadata()
                .is_ok_and(|metadata| metadata.len() == distribution.archive_size)
            && sha256_file_checked(&archive_path, Some(&cancellation))?
                == distribution.archive_checksum;
        if !cached {
            journal.record(
                OperationState::Running,
                "download",
                "Downloading the official Codex archive",
                Some(0.2),
            )?;
            self.download_asset(
                &distribution.archive_url,
                &archive_path,
                distribution.archive_size,
                &cancellation,
            )
            .await?;
        }
        journal.record(
            OperationState::Running,
            "verify",
            "Verifying the official Codex SHA-256 checksum",
            Some(0.4),
        )?;
        let actual = sha256_file_checked(&archive_path, Some(&cancellation))?;
        if actual != distribution.archive_checksum {
            return Err(TorbenError::new(
                "archive_hash_mismatch",
                "The Codex archive does not match its official release digest.",
            )
            .with_detail("expected", distribution.archive_checksum)
            .with_detail("actual", actual));
        }
        let sigstore_material = if let Some(sigstore) = &distribution.sigstore {
            let bundle = self
                .fetch_limited(
                    &sigstore.url,
                    MAX_BUNDLE_BYTES,
                    RequestKind::Asset,
                    Some(&cancellation),
                )
                .await?;
            if u64::try_from(bundle.len()).unwrap_or(u64::MAX) != sigstore.size
                || sha256_bytes(&bundle) != sigstore.checksum
            {
                return Err(TorbenError::new(
                    "codex_sigstore_bundle_invalid",
                    "The Codex Sigstore bundle does not match its official release metadata.",
                ));
            }
            Some((bundle, sigstore.clone()))
        } else {
            None
        };
        cancellation.check()?;
        let staging =
            paths
                .staging_dir()
                .join(format!("install-{}-{}", app_id, journal.operation_id()));
        let payload = staging.join("payload");
        let runtime = staging.join("runtime");
        std::fs::create_dir_all(&payload).map_err(io_error)?;
        std::fs::create_dir_all(&runtime).map_err(io_error)?;
        journal.record(
            OperationState::Running,
            "extract",
            "Extracting Codex into transaction staging",
            Some(0.68),
        )?;
        let archive = archive_path.clone();
        let archive_kind = distribution.archive_kind;
        let extraction_target = payload.clone();
        let extraction_cancellation = cancellation.clone();
        tokio::task::spawn_blocking(move || {
            extract_archive_contents(
                &archive,
                archive_kind,
                &extraction_target,
                &extraction_cancellation,
            )
        })
        .await
        .map_err(archive_task_error)??;
        let binary = find_payload_binary(&payload, &distribution.binary_name)?;
        let managed_binary = runtime.join(managed_binary_name());
        std::fs::rename(&binary, &managed_binary).map_err(io_error)?;
        ensure_regular_file(&managed_binary)?;
        if let Some((bundle, sigstore)) = sigstore_material {
            journal.record(
                OperationState::Running,
                "verify",
                "Verifying the extracted Codex binary with Fulcio and Rekor evidence",
                Some(0.78),
            )?;
            let binary_digest = sha256_file_checked(&managed_binary, Some(&cancellation))?;
            self.sigstore_verifier.verify(
                &binary_digest,
                &bundle,
                &sigstore.identity,
                &sigstore.issuer,
            )?;
        }
        journal.record(
            OperationState::Running,
            "health_check",
            "Checking the extracted Codex command with isolated state",
            Some(0.86),
        )?;
        self.health_check_path(&runtime, version).await?;
        cancellation.check()?;
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        std::fs::rename(&runtime, &final_path).map_err(|error| {
            TorbenError::new(
                "install_commit_failed",
                "Could not atomically commit the Codex installation.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let _ = std::fs::remove_dir_all(&staging);
        Ok(InstallRecord {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: plan.source_id.clone(),
            scope: InstallScope::Managed,
            install_path: final_path.display().to_string(),
            installed_at: timestamp_seconds(),
            health: "healthy".to_owned(),
        })
    }

    pub async fn health_check(&self, record: &InstallRecord) -> TorbenResult<()> {
        self.health_check_path(Path::new(&record.install_path), &record.version)
            .await
    }

    pub fn command_path(&self, install_path: &Path, command: &str) -> TorbenResult<PathBuf> {
        if command != "codex" {
            return Err(TorbenError::new(
                "unsupported_command",
                "The Codex plugin does not expose this command.",
            )
            .with_detail("command", command));
        }
        let path = install_path.join(managed_binary_name());
        ensure_regular_file(&path)?;
        Ok(path)
    }

    pub async fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let names: &[&str] = if cfg!(windows) {
            &["codex.exe", "codex.cmd"]
        } else {
            &["codex"]
        };
        let mut candidates = BTreeSet::new();
        for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            for name in names {
                candidates.insert(directory.join(name));
            }
        }
        let mut records = Vec::new();
        for candidate in candidates {
            let Ok(canonical) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if canonical.starts_with(managed_root) || ensure_regular_file(&canonical).is_err() {
                continue;
            }
            let Ok(version) = isolated_version(&canonical).await else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("codex")?,
                version,
                source_id: SourceId::new("codex.external")?,
                scope: InstallScope::External,
                install_path: canonical.display().to_string(),
                installed_at: String::new(),
                health: "healthy".to_owned(),
            });
        }
        Ok(records)
    }

    #[allow(clippy::too_many_lines)]
    async fn validate_install_plan(
        &self,
        plan: &InstallPlan,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<CodexDistribution> {
        if app_id.as_str() != "codex"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new("codex.official")?
        {
            return Err(invalid_plan("identity or source owner"));
        }
        let expected = self.distribution(version).await?;
        if plan.metadata.get("target") != Some(&expected.target)
            || plan.metadata.get("releaseTag") != Some(&expected.tag)
        {
            return Err(invalid_plan("target or release metadata"));
        }
        let valid = if let Some(sigstore) = &expected.sigstore {
            matches!(
                plan.steps.as_slice(),
                [
                    InstallStep::Download { url, destination_name },
                    InstallStep::VerifySha256 { archive_name, expected: checksum },
                    InstallStep::VerifySigstoreBundle { archive_name: signed_archive, bundle_url, certificate_identity, oidc_issuer },
                    InstallStep::ExtractArchive { archive_name: extracted_archive, strip_components },
                    InstallStep::HealthCheck { executable, arguments, expected_output },
                    InstallStep::CreateShims { commands },
                ] if Url::parse(url).ok().as_ref() == Some(&expected.archive_url)
                    && destination_name == &expected.archive_name
                    && archive_name == &expected.archive_name
                    && checksum.to_ascii_lowercase() == expected.archive_checksum
                    && signed_archive == &expected.binary_name
                    && Url::parse(bundle_url).ok().as_ref() == Some(&sigstore.url)
                    && certificate_identity == &sigstore.identity
                    && oidc_issuer == &sigstore.issuer
                    && extracted_archive == &expected.archive_name
                    && *strip_components == 0
                    && executable == "codex"
                    && arguments.as_slice() == ["--version"]
                    && expected_output == &version.to_string()
                    && commands.as_slice() == ["codex"]
            )
        } else {
            matches!(
                plan.steps.as_slice(),
                [
                    InstallStep::Download { url, destination_name },
                    InstallStep::VerifySha256 { archive_name, expected: checksum },
                    InstallStep::ExtractArchive { archive_name: extracted_archive, strip_components },
                    InstallStep::HealthCheck { executable, arguments, expected_output },
                    InstallStep::CreateShims { commands },
                ] if Url::parse(url).ok().as_ref() == Some(&expected.archive_url)
                    && destination_name == &expected.archive_name
                    && archive_name == &expected.archive_name
                    && checksum.to_ascii_lowercase() == expected.archive_checksum
                    && extracted_archive == &expected.archive_name
                    && *strip_components == 0
                    && executable == "codex"
                    && arguments.as_slice() == ["--version"]
                    && expected_output == &version.to_string()
                    && commands.as_slice() == ["codex"]
            )
        };
        if !valid {
            return Err(invalid_plan("official distribution details or step order"));
        }
        Ok(expected)
    }

    async fn health_check_path(
        &self,
        install_path: &Path,
        version: &ExactVersion,
    ) -> TorbenResult<()> {
        let executable = self.command_path(install_path, "codex")?;
        let actual = isolated_version(&executable).await?;
        if &actual != version {
            return Err(TorbenError::new(
                "health_check_version_mismatch",
                "The managed Codex version does not match the requested version.",
            )
            .with_detail("expected", version.to_string())
            .with_detail("actual", actual.to_string()));
        }
        Ok(())
    }

    async fn release(&self, version: &ExactVersion) -> TorbenResult<GitHubRelease> {
        let url = self
            .releases_url
            .join(&format!("tags/rust-v{version}"))
            .map_err(url_error)?;
        let release: GitHubRelease = self.fetch_json(&url).await?;
        let actual = validate_release(&release)?;
        if &actual != version {
            return Err(version_not_found(&version.to_string()));
        }
        Ok(release)
    }

    async fn fetch_json<T: for<'de> Deserialize<'de>>(&self, url: &Url) -> TorbenResult<T> {
        let bytes = self
            .fetch_limited(url, MAX_METADATA_BYTES, RequestKind::Api, None)
            .await?;
        serde_json::from_slice(&bytes).map_err(|error| {
            TorbenError::new(
                "codex_metadata_invalid",
                "The official Codex release metadata is not valid JSON.",
            )
            .with_detail("reason", error.to_string())
        })
    }

    async fn fetch_limited(
        &self,
        url: &Url,
        maximum: u64,
        kind: RequestKind,
        cancellation: Option<&CancellationProbe>,
    ) -> TorbenResult<Vec<u8>> {
        let response = await_with_cancellation(
            async {
                self.client
                    .get(url.clone())
                    .send()
                    .await
                    .map_err(network_error)
            },
            cancellation,
        )
        .await?;
        self.validate_response(&response, url, kind)?;
        let response = response.error_for_status().map_err(network_error)?;
        if response.content_length().is_some_and(|size| size > maximum) {
            return Err(resource_too_large(maximum));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = await_with_cancellation(
            async { stream.next().await.transpose().map_err(network_error) },
            cancellation,
        )
        .await?
        {
            if let Some(cancellation) = cancellation {
                cancellation.check()?;
            }
            let next = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| resource_too_large(maximum))?;
            if u64::try_from(next).unwrap_or(u64::MAX) > maximum {
                return Err(resource_too_large(maximum));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn download_asset(
        &self,
        url: &Url,
        destination: &Path,
        expected_size: u64,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<()> {
        let response = await_with_cancellation(
            async {
                self.client
                    .get(url.clone())
                    .send()
                    .await
                    .map_err(network_error)
            },
            Some(cancellation),
        )
        .await?;
        self.validate_response(&response, url, RequestKind::Asset)?;
        let response = response.error_for_status().map_err(network_error)?;
        if response
            .content_length()
            .is_some_and(|size| size != expected_size)
        {
            return Err(size_mismatch(expected_size, response.content_length()));
        }
        let partial = destination.with_extension("partial");
        let mut file = tokio::fs::File::create(&partial).await.map_err(io_error)?;
        let mut received = 0_u64;
        let mut stream = response.bytes_stream();
        let result = async {
            while let Some(chunk) = await_with_cancellation(
                async { stream.next().await.transpose().map_err(network_error) },
                Some(cancellation),
            )
            .await?
            {
                received = received
                    .checked_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
                    .ok_or_else(|| resource_too_large(MAX_ARCHIVE_BYTES))?;
                if received > expected_size || received > MAX_ARCHIVE_BYTES {
                    return Err(size_mismatch(expected_size, Some(received)));
                }
                file.write_all(&chunk).await.map_err(io_error)?;
            }
            file.flush().await.map_err(io_error)?;
            file.sync_all().await.map_err(io_error)?;
            drop(file);
            if received != expected_size {
                return Err(size_mismatch(expected_size, Some(received)));
            }
            if destination.exists() {
                tokio::fs::remove_file(destination)
                    .await
                    .map_err(io_error)?;
            }
            tokio::fs::rename(&partial, destination)
                .await
                .map_err(io_error)
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&partial).await;
        }
        result
    }

    fn validate_response(
        &self,
        response: &reqwest::Response,
        requested: &Url,
        kind: RequestKind,
    ) -> TorbenResult<()> {
        if self.fixture_mode {
            return same_origin(response.url(), requested)
                .then_some(())
                .ok_or_else(|| unexpected_origin(response.url()));
        }
        let valid = match kind {
            RequestKind::Api => {
                response.url().scheme() == "https"
                    && response.url().host_str() == Some("api.github.com")
                    && response
                        .url()
                        .path()
                        .starts_with("/repos/openai/codex/releases")
            }
            RequestKind::Asset => {
                response.url().scheme() == "https"
                    && matches!(
                        response.url().host_str(),
                        Some("github.com" | "release-assets.githubusercontent.com")
                    )
            }
        };
        valid
            .then_some(())
            .ok_or_else(|| unexpected_origin(response.url()))
    }
}

fn validate_release(release: &GitHubRelease) -> TorbenResult<ExactVersion> {
    if release.draft || release.prerelease {
        return Err(metadata_invalid("release stability"));
    }
    let raw = release
        .tag_name
        .strip_prefix("rust-v")
        .ok_or_else(|| metadata_invalid("release tag"))?;
    let version = ExactVersion::from_str(raw)?;
    if !version.as_semver().pre.is_empty()
        || !version.as_semver().build.is_empty()
        || release.name != version.to_string()
        || release.html_url
            != format!(
                "https://github.com/openai/codex/releases/tag/{}",
                release.tag_name
            )
    {
        return Err(metadata_invalid("release identity"));
    }
    Ok(version)
}

fn distribution_from(
    release: &GitHubRelease,
    version: &ExactVersion,
    target: &TargetAsset,
    fixture_mode: bool,
) -> TorbenResult<CodexDistribution> {
    let archive = select_asset(release, &target.archive_name, fixture_mode)?;
    if archive.size > MAX_ARCHIVE_BYTES {
        return Err(metadata_invalid("archive size"));
    }
    let sigstore = target
        .sigstore_name
        .as_deref()
        .map(|name| {
            let asset = select_asset(release, name, fixture_mode)?;
            if asset.size > MAX_BUNDLE_BYTES {
                return Err(metadata_invalid("Sigstore bundle size"));
            }
            Ok(CodexSigstoreBundle {
                url: asset.url,
                checksum: asset.checksum,
                size: asset.size,
                identity: format!(
                    "https://github.com/openai/codex/.github/workflows/rust-release.yml@refs/tags/{}",
                    release.tag_name
                ),
                issuer: SIGSTORE_ISSUER.to_owned(),
            })
        })
        .transpose()?;
    Ok(CodexDistribution {
        version: version.clone(),
        released_at: release.published_at.clone(),
        tag: release.tag_name.clone(),
        target: target.target.to_owned(),
        archive_name: target.archive_name.clone(),
        archive_url: archive.url,
        archive_checksum: archive.checksum,
        archive_size: archive.size,
        archive_kind: target.archive_kind,
        binary_name: target.binary_name.clone(),
        sigstore,
    })
}

struct SelectedAsset {
    url: Url,
    checksum: String,
    size: u64,
}

fn select_asset(
    release: &GitHubRelease,
    name: &str,
    fixture_mode: bool,
) -> TorbenResult<SelectedAsset> {
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| {
            TorbenError::new(
                "codex_archive_missing",
                "The official Codex release has no matching target asset.",
            )
            .with_detail("asset", name)
        })?;
    let url = Url::parse(&asset.browser_download_url).map_err(url_error)?;
    let expected_path = format!(
        "/openai/codex/releases/download/{}/{}",
        release.tag_name, name
    );
    let valid_url = if fixture_mode {
        url.path().ends_with(&format!("/{name}"))
    } else {
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.path() == expected_path
    };
    let checksum = asset
        .digest
        .as_deref()
        .and_then(|value| value.strip_prefix("sha256:"))
        .filter(|value| is_hex(value, 64))
        .ok_or_else(|| metadata_invalid("asset digest"))?
        .to_ascii_lowercase();
    if asset.state != "uploaded" || asset.size == 0 || !valid_url {
        return Err(metadata_invalid("asset identity"));
    }
    Ok(SelectedAsset {
        url,
        checksum,
        size: asset.size,
    })
}

fn current_target_asset() -> TorbenResult<TargetAsset> {
    let (target, extension, kind, sigstore) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => (
            "x86_64-pc-windows-msvc",
            ".exe.zip",
            ArchiveKind::Zip,
            false,
        ),
        ("windows", "aarch64") => (
            "aarch64-pc-windows-msvc",
            ".exe.zip",
            ArchiveKind::Zip,
            false,
        ),
        ("macos", "x86_64") => ("x86_64-apple-darwin", ".tar.gz", ArchiveKind::TarGz, false),
        ("macos", "aarch64") => ("aarch64-apple-darwin", ".tar.gz", ArchiveKind::TarGz, false),
        ("linux", "x86_64") => (
            "x86_64-unknown-linux-musl",
            ".tar.gz",
            ArchiveKind::TarGz,
            true,
        ),
        ("linux", "aarch64") => (
            "aarch64-unknown-linux-musl",
            ".tar.gz",
            ArchiveKind::TarGz,
            true,
        ),
        (os, architecture) => {
            return Err(TorbenError::new(
                "platform_not_supported",
                "Codex is not available for this platform target.",
            )
            .with_detail("os", os)
            .with_detail("architecture", architecture));
        }
    };
    let binary_name = format!(
        "codex-{target}{}",
        if target.contains("windows") {
            ".exe"
        } else {
            ""
        }
    );
    Ok(TargetAsset {
        target,
        archive_name: format!("codex-{target}{extension}"),
        binary_name,
        archive_kind: kind,
        sigstore_name: sigstore.then(|| format!("codex-{target}.sigstore")),
    })
}

fn find_payload_binary(payload: &Path, name: &str) -> TorbenResult<PathBuf> {
    let direct = payload.join(name);
    if ensure_regular_file(&direct).is_ok() {
        return Ok(direct);
    }
    let directories = std::fs::read_dir(payload)
        .map_err(io_error)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            path.is_dir().then_some(path)
        })
        .collect::<Vec<_>>();
    if directories.len() == 1 {
        let nested = directories[0].join(name);
        if ensure_regular_file(&nested).is_ok() {
            return Ok(nested);
        }
    }
    Err(TorbenError::new(
        "archive_layout_invalid",
        "The Codex archive does not contain the exact target binary.",
    )
    .with_detail("binary", name))
}

async fn isolated_version(executable: &Path) -> TorbenResult<ExactVersion> {
    let isolated_home = std::env::temp_dir().join(format!(
        "torben-codex-health-{}-{}",
        std::process::id(),
        timestamp_nanos()
    ));
    std::fs::create_dir_all(&isolated_home).map_err(io_error)?;
    let result = tokio::time::timeout(
        PROCESS_TIMEOUT,
        process::async_command(executable)
            .arg("--version")
            .env("CODEX_HOME", &isolated_home)
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let cleanup = std::fs::remove_dir_all(&isolated_home);
    let output = result
        .map_err(|_| {
            TorbenError::new("health_check_timeout", "The Codex version check timed out.")
        })?
        .map_err(|error| {
            TorbenError::new(
                "health_check_start_failed",
                "Could not start the Codex version check.",
            )
            .with_detail("path", executable.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
    cleanup.map_err(io_error)?;
    if !output.status.success() {
        return Err(TorbenError::new(
            "health_check_failed",
            "The Codex version command returned an error.",
        )
        .with_detail("status", output.status.to_string()));
    }
    parse_codex_version(&String::from_utf8_lossy(&output.stdout))
}

fn parse_codex_version(output: &str) -> TorbenResult<ExactVersion> {
    let line = output.lines().next().map(str::trim).unwrap_or_default();
    let raw = line
        .strip_prefix("codex-cli ")
        .or_else(|| line.strip_prefix("codex "))
        .unwrap_or(line);
    ExactVersion::from_str(raw).map_err(|_| {
        TorbenError::new(
            "health_check_output_invalid",
            "Codex returned an invalid version string.",
        )
        .with_detail("actual", line)
    })
}

async fn await_with_cancellation<F, T>(
    future: F,
    cancellation: Option<&CancellationProbe>,
) -> TorbenResult<T>
where
    F: std::future::Future<Output = TorbenResult<T>>,
{
    let Some(cancellation) = cancellation else {
        return future.await;
    };
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            () = tokio::time::sleep(POLL_INTERVAL) => cancellation.check()?,
        }
    }
}

fn ensure_regular_file(path: &Path) -> TorbenResult<()> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TorbenError::new(
            "codex_path_invalid",
            "A Codex transaction file is not a regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    Ok(())
}

fn managed_binary_name() -> &'static str {
    if cfg!(windows) { "codex.exe" } else { "codex" }
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn timestamp_seconds() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
        .to_string()
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn version_not_found(requested: &str) -> TorbenError {
    TorbenError::new(
        "version_not_found",
        "The requested Codex version is not a supported stable release.",
    )
    .with_detail("requested", requested)
}

fn metadata_invalid(field: &str) -> TorbenError {
    TorbenError::new(
        "codex_metadata_invalid",
        "The official Codex release metadata contains an invalid field.",
    )
    .with_detail("field", field)
}

fn invalid_plan(reason: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The Codex plugin returned an unsafe or inconsistent install plan.",
    )
    .with_detail("reason", reason)
}

fn resource_too_large(maximum: u64) -> TorbenError {
    TorbenError::new(
        "codex_resource_too_large",
        "A Codex release response exceeds the allowed size.",
    )
    .with_detail("maximumBytes", maximum.to_string())
}

fn size_mismatch(expected: u64, actual: Option<u64>) -> TorbenError {
    TorbenError::new(
        "archive_size_mismatch",
        "The Codex archive size does not match official metadata.",
    )
    .with_detail("expectedBytes", expected.to_string())
    .with_detail(
        "actualBytes",
        actual.map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
    )
}

fn unexpected_origin(url: &Url) -> TorbenError {
    TorbenError::new(
        "unexpected_download_origin",
        "A Codex request changed to an untrusted network origin.",
    )
    .with_detail("url", url.to_string())
}

fn archive_task_error(error: tokio::task::JoinError) -> TorbenError {
    TorbenError::new(
        "archive_task_failed",
        "The Codex archive extraction task failed.",
    )
    .with_detail("reason", error.to_string())
}

fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new("network_error", "An official Codex release request failed.")
        .with_detail("reason", error.to_string())
}

fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new("codex_url_invalid", "An official Codex URL is invalid.")
        .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new("filesystem_error", "A Codex filesystem operation failed.")
        .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        io::{Read as _, Write as _},
        net::TcpListener,
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    #[cfg(windows)]
    use std::collections::VecDeque;

    #[cfg(windows)]
    use sha2::{Digest, Sha256};
    #[cfg(windows)]
    use tempfile::tempdir;
    #[cfg(windows)]
    use torben_contracts::AppId;
    use torben_contracts::ExactVersion;

    use super::*;
    #[cfg(windows)]
    use crate::{TorbenCore, bundled_shim::BundledShim, node_plugin::BundledPlugin};

    struct AcceptingSigstoreVerifier;

    impl CodexSigstoreVerifier for AcceptingSigstoreVerifier {
        fn verify(
            &self,
            _sha256: &str,
            _bundle: &[u8],
            _certificate_identity: &str,
            _oidc_issuer: &str,
        ) -> TorbenResult<()> {
            Ok(())
        }
    }

    #[test]
    fn validates_stable_release_and_exact_windows_asset() {
        let version = ExactVersion::from_str("0.149.1").unwrap();
        let release = release_fixture(
            &version,
            "https://github.com/openai/codex/releases/download/rust-v0.149.1/codex-x86_64-pc-windows-msvc.exe.zip",
            &"11".repeat(32),
            105_000_000,
        );
        let target = TargetAsset {
            target: "x86_64-pc-windows-msvc",
            archive_name: "codex-x86_64-pc-windows-msvc.exe.zip".to_owned(),
            binary_name: "codex-x86_64-pc-windows-msvc.exe".to_owned(),
            archive_kind: ArchiveKind::Zip,
            sigstore_name: None,
        };

        assert_eq!(validate_release(&release).unwrap(), version);
        let distribution = distribution_from(&release, &version, &target, false).unwrap();
        assert_eq!(distribution.archive_checksum, "11".repeat(32));
        assert_eq!(distribution.target, "x86_64-pc-windows-msvc");
        assert!(distribution.sigstore.is_none());
    }

    #[test]
    fn linux_distribution_pins_the_release_workflow_identity() {
        let version = ExactVersion::from_str("0.149.1").unwrap();
        let mut release = release_fixture(
            &version,
            "https://github.com/openai/codex/releases/download/rust-v0.149.1/codex-x86_64-unknown-linux-musl.tar.gz",
            &"22".repeat(32),
            99_000_000,
        );
        release.assets.push(asset_fixture(
            "codex-x86_64-unknown-linux-musl.sigstore",
            "https://github.com/openai/codex/releases/download/rust-v0.149.1/codex-x86_64-unknown-linux-musl.sigstore",
            &"33".repeat(32),
            8_565,
        ));
        let target = TargetAsset {
            target: "x86_64-unknown-linux-musl",
            archive_name: "codex-x86_64-unknown-linux-musl.tar.gz".to_owned(),
            binary_name: "codex-x86_64-unknown-linux-musl".to_owned(),
            archive_kind: ArchiveKind::TarGz,
            sigstore_name: Some("codex-x86_64-unknown-linux-musl.sigstore".to_owned()),
        };

        let distribution = distribution_from(&release, &version, &target, false).unwrap();
        let sigstore = distribution.sigstore.unwrap();
        assert_eq!(sigstore.issuer, SIGSTORE_ISSUER);
        assert_eq!(
            sigstore.identity,
            "https://github.com/openai/codex/.github/workflows/rust-release.yml@refs/tags/rust-v0.149.1"
        );
    }

    #[test]
    fn rejects_missing_or_unhashed_target_asset() {
        let version = ExactVersion::from_str("0.149.1").unwrap();
        let mut release = release_fixture(
            &version,
            "https://github.com/openai/codex/releases/download/rust-v0.149.1/codex-x86_64-pc-windows-msvc.exe.zip",
            &"11".repeat(32),
            100,
        );
        release.assets[0].digest = None;
        let target = TargetAsset {
            target: "x86_64-pc-windows-msvc",
            archive_name: "codex-x86_64-pc-windows-msvc.exe.zip".to_owned(),
            binary_name: "codex-x86_64-pc-windows-msvc.exe".to_owned(),
            archive_kind: ArchiveKind::Zip,
            sigstore_name: None,
        };
        assert_eq!(
            distribution_from(&release, &version, &target, false)
                .unwrap_err()
                .code,
            "codex_metadata_invalid"
        );
    }

    #[test]
    fn parses_supported_codex_version_output() {
        assert_eq!(
            parse_codex_version("codex-cli 0.149.1\n")
                .unwrap()
                .to_string(),
            "0.149.1"
        );
        assert_eq!(
            parse_codex_version("codex 0.149.1\n").unwrap().to_string(),
            "0.149.1"
        );
        assert_eq!(
            parse_codex_version("not codex").unwrap_err().code,
            "health_check_output_invalid"
        );
    }

    #[test]
    fn malformed_legacy_sigstore_bundle_fails_closed() {
        let error = parse_legacy_cosign_bundle(&"00".repeat(32), b"{}").unwrap_err();
        assert_eq!(error.code, "codex_sigstore_bundle_invalid");
    }

    #[test]
    #[ignore = "requires an explicitly downloaded official Codex Sigstore bundle"]
    fn verifies_official_codex_01491_linux_bundle_by_digest() {
        let path = std::env::var_os("TORBEN_CODEX_SIGSTORE_FIXTURE")
            .expect("set TORBEN_CODEX_SIGSTORE_FIXTURE to the official bundle path");
        let bundle = std::fs::read(path).unwrap();
        let bundle = parse_legacy_cosign_bundle(
            "73dc5888888f411c1f0fa7b81d866e721dcc86b527ce8e3b2cf4708661e823ba",
            &bundle,
        )
        .unwrap();
        crate::python_sigstore::verify_production_sigstore_bundle(
            "73dc5888888f411c1f0fa7b81d866e721dcc86b527ce8e3b2cf4708661e823ba",
            &bundle,
            "https://github.com/openai/codex/.github/workflows/rust-release.yml@refs/tags/rust-v0.149.1",
            SIGSTORE_ISSUER,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn local_release_catalog_lists_supported_stable_versions() {
        let version = ExactVersion::from_str("0.149.1").unwrap();
        let target = current_target_asset().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = Url::parse(&format!("http://{address}/")).unwrap();
        let asset_url = base
            .join(&format!("assets/{}", target.archive_name))
            .unwrap();
        let mut release = release_fixture(&version, asset_url.as_str(), &"11".repeat(32), 100);
        if let Some(sigstore_name) = target.sigstore_name {
            let sigstore_url = base.join(&format!("assets/{sigstore_name}")).unwrap();
            release.assets.push(asset_fixture(
                &sigstore_name,
                sigstore_url.as_str(),
                &"22".repeat(32),
                100,
            ));
        }
        let routes = vec![
            (
                "/releases?per_page=20&page=1".to_owned(),
                serde_json::to_vec(&vec![release]).unwrap(),
            ),
            (
                "/releases?per_page=20&page=2".to_owned(),
                serde_json::to_vec(&Vec::<GitHubRelease>::new()).unwrap(),
            ),
        ];
        let server = spawn_concurrent_fixture_server(listener, routes);
        let provider = CodexProvider::with_fixture(
            base.join("releases/").unwrap(),
            Arc::new(AcceptingSigstoreVerifier),
        )
        .unwrap();

        let versions = provider.list_versions().await.unwrap();
        server.join().unwrap();

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, version);
        assert!(versions[0].recommended);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_fixture_completes_core_install_select_and_uninstall_transaction() {
        let root = tempdir().unwrap();
        let fixture_codex = compile_fixture_codex(root.path());
        let archive = codex_archive(&fixture_codex);
        let checksum = hex::encode(Sha256::digest(&archive));
        let version = ExactVersion::from_str("0.149.1").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = Url::parse(&format!("http://{address}/")).unwrap();
        let asset_url = base
            .join("assets/codex-x86_64-pc-windows-msvc.exe.zip")
            .unwrap();
        let release = release_fixture(
            &version,
            asset_url.as_str(),
            &checksum,
            u64::try_from(archive.len()).unwrap(),
        );
        let routes = vec![
            (
                "/releases/tags/rust-v0.149.1".to_owned(),
                serde_json::to_vec(&release).unwrap(),
            ),
            (
                "/assets/codex-x86_64-pc-windows-msvc.exe.zip".to_owned(),
                archive,
            ),
        ];
        let server = spawn_fixture_server(listener, routes);
        let provider = CodexProvider::with_fixture(
            base.join("releases/").unwrap(),
            Arc::new(AcceptingSigstoreVerifier),
        )
        .unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let final_path = paths.app_version_dir("codex", &version.to_string());
        let plugin_script = root.path().join("codex-plugin.cmd");
        std::fs::write(
            &plugin_script,
            windows_plugin_fixture_script(&version, &final_path, &asset_url, &checksum),
        )
        .unwrap();
        let mut core = TorbenCore::open(paths).unwrap();
        core.codex = provider;
        core.codex_plugin = BundledPlugin::codex_from_executable(plugin_script);
        core.bundled_shim = BundledShim::from_executable(fixture_codex);
        let app_id = AppId::new("codex").unwrap();

        let installed = core.install(&app_id, "current").await.unwrap();
        server.join().unwrap();
        core.select(&app_id, &version).await.unwrap();
        let command_available = core.executable_for(&app_id, "codex").unwrap().is_file();
        core.clear_selection(&app_id).unwrap();
        core.uninstall(&app_id, &version).await.unwrap();

        assert_eq!(installed.version, version);
        assert!(command_available);
        assert!(core.installed().unwrap().is_empty());
        assert!(!Path::new(&installed.install_path).exists());
    }

    fn release_fixture(
        version: &ExactVersion,
        asset_url: &str,
        checksum: &str,
        size: u64,
    ) -> GitHubRelease {
        let name = Url::parse(asset_url)
            .ok()
            .and_then(|url| {
                url.path_segments()
                    .and_then(Iterator::last)
                    .map(str::to_owned)
            })
            .unwrap();
        GitHubRelease {
            tag_name: format!("rust-v{version}"),
            name: version.to_string(),
            html_url: format!("https://github.com/openai/codex/releases/tag/rust-v{version}"),
            draft: false,
            prerelease: false,
            published_at: "2026-08-24T00:28:28Z".to_owned(),
            assets: vec![asset_fixture(&name, asset_url, checksum, size)],
        }
    }

    fn asset_fixture(name: &str, url: &str, checksum: &str, size: u64) -> GitHubAsset {
        GitHubAsset {
            name: name.to_owned(),
            state: "uploaded".to_owned(),
            size,
            digest: Some(format!("sha256:{checksum}")),
            browser_download_url: url.to_owned(),
        }
    }

    #[cfg(windows)]
    fn compile_fixture_codex(directory: &Path) -> PathBuf {
        let source = directory.join("fixture-codex.rs");
        let executable = directory.join("codex-x86_64-pc-windows-msvc.exe");
        std::fs::write(
            &source,
            r#"fn main() {
                assert!(std::env::var_os("CODEX_HOME").is_some());
                println!("codex-cli 0.149.1");
            }"#,
        )
        .unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = std::process::Command::new(rustc)
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fixture rustc failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        executable
    }

    #[cfg(windows)]
    fn codex_archive(executable: &Path) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer
            .start_file("codex-x86_64-pc-windows-msvc.exe", options)
            .unwrap();
        writer
            .write_all(&std::fs::read(executable).unwrap())
            .unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[cfg(windows)]
    fn windows_plugin_fixture_script(
        version: &ExactVersion,
        install_path: &Path,
        archive_url: &Url,
        checksum: &str,
    ) -> String {
        let initialize = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {
                "protocolVersion": torben_contracts::plugin::PLUGIN_PROTOCOL_VERSION,
                "pluginId": "app.torben.plugin.codex", "pluginVersion": env!("CARGO_PKG_VERSION"),
                "applications": [{
                    "id": "codex", "displayName": "Codex CLI", "summary": "fixture",
                    "categories": ["ai"], "capabilities": ["versions", "install", "select", "uninstall"],
                    "sources": [{"id": "codex.official", "displayName": "Official", "managed": true}]
                }]
            }
        }).to_string();
        let resolved = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "result": {"requested": "current", "resolved": version}
        })
        .to_string();
        let plan = serde_json::json!({
            "jsonrpc": "2.0", "id": 3,
            "result": {
                "appId": "codex", "version": version, "sourceId": "codex.official",
                "steps": [
                    {"type": "download", "url": archive_url, "destination_name": "codex-x86_64-pc-windows-msvc.exe.zip"},
                    {"type": "verify_sha256", "archive_name": "codex-x86_64-pc-windows-msvc.exe.zip", "expected": checksum},
                    {"type": "extract_archive", "archive_name": "codex-x86_64-pc-windows-msvc.exe.zip", "strip_components": 0},
                    {"type": "health_check", "executable": "codex", "arguments": ["--version"], "expected_output": version},
                    {"type": "create_shims", "commands": ["codex"]}
                ],
                "metadata": {"target": "x86_64-pc-windows-msvc", "releaseTag": "rust-v0.149.1"}
            }
        }).to_string();
        let health = serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": {"healthy": true, "actualVersion": version, "message": "healthy"}
        })
        .to_string();
        let uninstall = serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": {"appId": "codex", "version": version, "sourceId": "codex.official", "installPath": install_path.display().to_string(), "preserveUserData": true}
        }).to_string();
        format!(
            "@echo off\r\n:loop\r\nset request=\r\nset /p request=\r\nif errorlevel 1 exit /b 0\r\necho %request%| findstr /c:\"initialize\" >nul && (echo {initialize}& goto loop)\r\necho %request%| findstr /c:\"version.resolve\" >nul && (echo {resolved}& goto loop)\r\necho %request%| findstr /c:\"uninstall.plan\" >nul && (echo {uninstall}& goto loop)\r\necho %request%| findstr /c:\"install.plan\" >nul && (echo {plan}& goto loop)\r\necho %request%| findstr /c:\"health.check\" >nul && (echo {health}& goto loop)\r\nexit /b 1\r\n"
        )
    }

    #[cfg(windows)]
    fn spawn_fixture_server(
        listener: TcpListener,
        routes: Vec<(String, Vec<u8>)>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let mut routes = routes.into_iter().collect::<VecDeque<_>>();
            while let Some((expected_path, body)) = routes.pop_front() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap();
                assert_eq!(path, expected_path);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        })
    }

    fn spawn_concurrent_fixture_server(
        listener: TcpListener,
        routes: Vec<(String, Vec<u8>)>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let expected = routes.into_iter().collect::<HashMap<_, _>>();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut requests = Vec::new();
            while requests.len() < expected.len() && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut request = [0_u8; 4096];
                        let read = stream.read(&mut request).unwrap();
                        let request = String::from_utf8_lossy(&request[..read]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap()
                            .to_owned();
                        requests.push((path, stream));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
            assert_eq!(
                requests.len(),
                expected.len(),
                "Codex catalog pages were not requested concurrently"
            );
            for (path, mut stream) in requests {
                let body = expected
                    .get(&path)
                    .unwrap_or_else(|| panic!("unexpected path: {path}"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        })
    }
}

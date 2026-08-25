use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    process::Stdio,
    str::FromStr,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};
use torben_contracts::{
    ExactVersion, OperationId, PluginId, TorbenError, TorbenResult,
    plugin::{
        JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, PLUGIN_PROTOCOL_VERSION,
        PLUGIN_REGISTRY_SCHEMA_VERSION, PluginCapability, PluginManifest, PluginOperationEvent,
        PluginRegistry, PluginRegistryEntry, PluginRegistryPublisher, PluginTarget, method,
    },
};

const MAX_SAFE_REGISTRY_SEQUENCE: u64 = 9_007_199_254_740_991;
const MAX_OPERATION_EVENTS_PER_CALL: usize = 1_024;
const MAX_PERMISSION_ENTRIES: usize = 64;
const MAX_PERMISSION_VALUE_LENGTH: usize = 253;
const MAX_PORTABLE_PATH_LENGTH: usize = 4_096;
const MAX_PORTABLE_COMPONENT_LENGTH: usize = 255;

#[derive(Debug, Clone)]
pub struct VerifiedPlugin {
    pub manifest: PluginManifest,
    pub executable: PathBuf,
}

pub struct PluginVerifier {
    registry_key: Option<VerifyingKey>,
    developer_mode: bool,
}

impl PluginVerifier {
    pub fn official(registry_key: VerifyingKey) -> Self {
        Self {
            registry_key: Some(registry_key),
            developer_mode: false,
        }
    }

    pub fn developer_mode() -> Self {
        Self {
            registry_key: None,
            developer_mode: true,
        }
    }

    /// Validates a plugin manifest and its executable for the current target.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest, target, signature, path, or executable hash is invalid.
    pub fn verify(&self, manifest_path: &Path) -> TorbenResult<VerifiedPlugin> {
        let manifest_bytes = std::fs::read(manifest_path).map_err(|error| io_error(&error))?;
        let manifest: PluginManifest =
            serde_json::from_slice(&manifest_bytes).map_err(|error| {
                TorbenError::new(
                    "plugin_manifest_invalid",
                    "The plugin manifest is not valid JSON.",
                )
                .with_detail("reason", error.to_string())
            })?;
        self.verify_manifest(&manifest)?;
        let plugin_target = current_plugin_target(&manifest)?;
        let relative = safe_plugin_executable_path(&plugin_target.executable)?;
        let root = manifest_path.parent().ok_or_else(|| {
            TorbenError::new(
                "plugin_manifest_path_invalid",
                "The plugin manifest has no parent directory.",
            )
        })?;
        let executable = root.join(relative);
        let actual_hash = sha256_file(&executable)?;
        if actual_hash != plugin_target.sha256.to_ascii_lowercase() {
            return Err(TorbenError::new(
                "plugin_hash_mismatch",
                "The plugin executable does not match its manifest hash.",
            )
            .with_detail("expected", &plugin_target.sha256)
            .with_detail("actual", actual_hash));
        }
        Ok(VerifiedPlugin {
            manifest,
            executable,
        })
    }

    fn verify_manifest(&self, manifest: &PluginManifest) -> TorbenResult<()> {
        if manifest.revoked {
            return Err(TorbenError::new(
                "plugin_revoked",
                "This plugin version has been revoked by its publisher.",
            ));
        }
        if manifest.protocol_version != PLUGIN_PROTOCOL_VERSION {
            return Err(TorbenError::new(
                "plugin_protocol_incompatible",
                "The plugin protocol version is incompatible with this host.",
            )
            .with_detail("pluginProtocol", manifest.protocol_version.to_string())
            .with_detail("hostProtocol", PLUGIN_PROTOCOL_VERSION.to_string()));
        }
        let host_version = ExactVersion::from_str(env!("CARGO_PKG_VERSION"))?;
        if manifest.minimum_host_version > host_version {
            return Err(TorbenError::new(
                "plugin_host_version_incompatible",
                "The plugin requires a newer Torben App host.",
            )
            .with_detail(
                "minimumHostVersion",
                manifest.minimum_host_version.to_string(),
            )
            .with_detail("hostVersion", host_version.to_string()));
        }
        if !self.developer_mode {
            self.verify_signature(manifest)?;
        }
        validate_manifest_declarations(manifest)?;
        let mut targets = BTreeSet::new();
        let mut executable_paths = BTreeSet::new();
        for target in &manifest.targets {
            if target.target.trim().is_empty() || !targets.insert(target.target.as_str()) {
                return Err(TorbenError::new(
                    "plugin_target_ambiguous",
                    "The plugin manifest contains a duplicate or empty target.",
                ));
            }
            let executable_path = safe_plugin_executable_path(&target.executable)?;
            if !executable_paths.insert(executable_path.to_string_lossy().to_ascii_lowercase()) {
                return Err(TorbenError::new(
                    "plugin_target_ambiguous",
                    "The plugin manifest reuses an executable path across targets.",
                ));
            }
            let hash = hex::decode(&target.sha256).map_err(|error| {
                TorbenError::new(
                    "plugin_hash_invalid",
                    "A plugin executable hash is not valid SHA-256.",
                )
                .with_detail("reason", error.to_string())
            })?;
            if hash.len() != 32 {
                return Err(TorbenError::new(
                    "plugin_hash_invalid",
                    "A plugin executable hash is not valid SHA-256.",
                ));
            }
        }
        Ok(())
    }

    fn verify_signature(&self, manifest: &PluginManifest) -> TorbenResult<()> {
        let key = self.registry_key.as_ref().ok_or_else(|| {
            TorbenError::new(
                "plugin_registry_key_missing",
                "No official plugin registry key is configured.",
            )
        })?;
        let encoded = manifest.signature.as_deref().ok_or_else(|| {
            TorbenError::new(
                "plugin_signature_missing",
                "The official plugin manifest is unsigned.",
            )
        })?;
        let signature_bytes = STANDARD.decode(encoded).map_err(|error| {
            TorbenError::new(
                "plugin_signature_invalid",
                "The plugin signature is not valid base64.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let signature = Signature::try_from(signature_bytes.as_slice()).map_err(|error| {
            TorbenError::new(
                "plugin_signature_invalid",
                "The plugin signature has an invalid length.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let mut unsigned = manifest.clone();
        unsigned.signature = None;
        let payload = serde_json::to_vec(&unsigned).map_err(|error| {
            TorbenError::internal("Could not serialize the plugin signature payload.")
                .with_detail("reason", error.to_string())
        })?;
        key.verify_strict(&payload, &signature).map_err(|error| {
            TorbenError::new(
                "plugin_signature_invalid",
                "The plugin publisher signature is invalid.",
            )
            .with_detail("reason", error.to_string())
        })
    }
}

fn validate_manifest_declarations(manifest: &PluginManifest) -> TorbenResult<()> {
    for (index, capability) in manifest.capabilities.iter().enumerate() {
        if manifest.capabilities[..index].contains(capability) {
            return Err(TorbenError::new(
                "plugin_capability_duplicate",
                "The plugin manifest declares the same capability more than once.",
            )
            .with_detail("capability", capability_name(capability)));
        }
    }

    validate_permission_values(
        "networkDomains",
        &manifest.permissions.network_domains,
        valid_network_domain,
    )?;
    validate_permission_values(
        "filesystemRoots",
        &manifest.permissions.filesystem_roots,
        |value| {
            matches!(
                value,
                "managed_app_library" | "download_cache" | "staging" | "plugin_data"
            )
        },
    )?;
    validate_permission_values(
        "externalCommands",
        &manifest.permissions.external_commands,
        valid_external_command,
    )?;
    validate_permission_values(
        "packageManagers",
        &manifest.permissions.package_managers,
        |value| matches!(value, "winget" | "homebrew" | "apt" | "dnf"),
    )
}

fn validate_permission_values(
    permission: &str,
    values: &[String],
    is_valid: impl Fn(&str) -> bool,
) -> TorbenResult<()> {
    if values.len() > MAX_PERMISSION_ENTRIES {
        return Err(TorbenError::new(
            "plugin_permission_limit_exceeded",
            "The plugin manifest declares too many permission values.",
        )
        .with_detail("permission", permission)
        .with_detail("limit", MAX_PERMISSION_ENTRIES.to_string()));
    }
    let mut unique = BTreeSet::new();
    for value in values {
        if value.len() > MAX_PERMISSION_VALUE_LENGTH || !is_valid(value) {
            return Err(TorbenError::new(
                "plugin_permission_invalid",
                "The plugin manifest contains an invalid permission value.",
            )
            .with_detail("permission", permission)
            .with_detail("value", value));
        }
        if !unique.insert(value.as_str()) {
            return Err(TorbenError::new(
                "plugin_permission_duplicate",
                "The plugin manifest declares the same permission value more than once.",
            )
            .with_detail("permission", permission)
            .with_detail("value", value));
        }
    }
    Ok(())
}

fn valid_network_domain(value: &str) -> bool {
    !value.is_empty()
        && value == value.to_ascii_lowercase()
        && !value.starts_with('.')
        && !value.ends_with('.')
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn valid_external_command(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
        && value != "."
        && value != ".."
}

#[derive(Debug, Clone)]
pub struct VerifiedRegistryPlugin {
    pub plugin: VerifiedPlugin,
    pub manifest_path: PathBuf,
    pub publisher_id: String,
}

#[derive(Debug, Clone)]
pub struct RegistryPluginSelection {
    pub entry: PluginRegistryEntry,
    pub publisher: PluginRegistryPublisher,
}

pub struct RegistryVerifier {
    root_key: VerifyingKey,
}

impl RegistryVerifier {
    pub fn new(root_key: VerifyingKey) -> Self {
        Self { root_key }
    }

    /// Creates a registry verifier from a base64 Ed25519 public key.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is not valid base64 or is not a valid Ed25519 public key.
    pub fn from_base64(encoded: &str) -> TorbenResult<Self> {
        Ok(Self::new(decode_verifying_key(
            encoded,
            "plugin_registry_key_invalid",
            "The official plugin registry key is invalid.",
        )?))
    }

    /// Verifies the signed registry, publisher authorization, manifest, and current target asset.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid signatures, ambiguous entries, revocation, incompatibility,
    /// unsafe paths, manifest mismatch, or executable hash mismatch.
    pub fn verify(
        &self,
        registry_path: &Path,
        plugin_id: &PluginId,
        version: Option<&ExactVersion>,
    ) -> TorbenResult<VerifiedRegistryPlugin> {
        let bytes = std::fs::read(registry_path).map_err(|error| io_error(&error))?;
        let registry = self.verify_registry_bytes(&bytes)?;
        let selection = self.select_plugin(&registry, plugin_id, version)?;
        let entry = &selection.entry;
        let publisher = &selection.publisher;
        let relative = safe_relative_path(&entry.manifest_path, "plugin_manifest_path_unsafe")?;
        let root = registry_path.parent().ok_or_else(|| {
            TorbenError::new(
                "plugin_registry_path_invalid",
                "The plugin registry has no parent directory.",
            )
        })?;
        let manifest_path = root.join(relative);
        let manifest_bytes = std::fs::read(&manifest_path).map_err(|error| io_error(&error))?;
        self.verify_manifest_bytes(&selection, &manifest_bytes)?;
        let publisher_key = decode_verifying_key(
            &publisher.public_key,
            "plugin_publisher_key_invalid",
            "The plugin publisher key is invalid.",
        )?;
        let plugin = PluginVerifier::official(publisher_key).verify(&manifest_path)?;
        Ok(VerifiedRegistryPlugin {
            plugin,
            manifest_path,
            publisher_id: publisher.id.clone(),
        })
    }

    /// Verifies and parses a registry snapshot without accessing package files.
    ///
    /// # Errors
    ///
    /// Returns an error when JSON, the root signature, schema, host version, sequence, or registry
    /// uniqueness constraints are invalid.
    pub fn verify_registry_bytes(&self, bytes: &[u8]) -> TorbenResult<PluginRegistry> {
        let registry: PluginRegistry = serde_json::from_slice(bytes).map_err(|error| {
            TorbenError::new(
                "plugin_registry_invalid",
                "The official plugin registry is not valid JSON.",
            )
            .with_detail("reason", error.to_string())
        })?;
        self.validate_registry(&registry)?;
        Ok(registry)
    }

    /// Selects one non-revoked plugin version from a verified registry snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry is invalid, the plugin is absent, a publisher or entry is
    /// revoked, or the manifest path is unsafe.
    pub fn select_plugin(
        &self,
        registry: &PluginRegistry,
        plugin_id: &PluginId,
        version: Option<&ExactVersion>,
    ) -> TorbenResult<RegistryPluginSelection> {
        self.validate_registry(registry)?;
        let mut candidates = registry
            .entries
            .iter()
            .filter(|entry| &entry.plugin_id == plugin_id)
            .filter(|entry| version.is_none_or(|version| &entry.version == version))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.version.cmp(&left.version));
        let entry = candidates.first().ok_or_else(|| {
            TorbenError::new(
                "plugin_registry_entry_not_found",
                "The requested plugin version is not present in the official registry.",
            )
            .with_detail("pluginId", plugin_id.to_string())
        })?;
        if entry.revoked {
            return Err(TorbenError::new(
                "plugin_registry_entry_revoked",
                "This official plugin registry entry has been revoked.",
            ));
        }
        let manifest_path =
            safe_relative_path(&entry.manifest_path, "plugin_manifest_path_unsafe")?;
        if manifest_path.file_name().and_then(std::ffi::OsStr::to_str) != Some("plugin.json") {
            return Err(TorbenError::new(
                "plugin_manifest_name_invalid",
                "A plugin package manifest must be named plugin.json.",
            ));
        }
        let publisher = registry
            .publishers
            .iter()
            .find(|publisher| publisher.id == entry.publisher_id)
            .ok_or_else(|| {
                TorbenError::new(
                    "plugin_registry_publisher_missing",
                    "The plugin registry entry references an unknown publisher.",
                )
            })?;
        if publisher.revoked {
            return Err(TorbenError::new(
                "plugin_registry_publisher_revoked",
                "The plugin publisher has been revoked by the official registry.",
            ));
        }
        Ok(RegistryPluginSelection {
            entry: (*entry).clone(),
            publisher: publisher.clone(),
        })
    }

    /// Verifies a registry-pinned publisher manifest before its executable is downloaded.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest hash, JSON, publisher signature, identity, target, or
    /// executable path is invalid.
    pub fn verify_manifest_bytes(
        &self,
        selection: &RegistryPluginSelection,
        bytes: &[u8],
    ) -> TorbenResult<PluginManifest> {
        let actual_hash = hex::encode(Sha256::digest(bytes));
        if actual_hash != selection.entry.manifest_sha256.to_ascii_lowercase() {
            return Err(TorbenError::new(
                "plugin_registry_manifest_hash_mismatch",
                "The plugin manifest does not match its signed registry entry.",
            ));
        }
        let manifest: PluginManifest = serde_json::from_slice(bytes).map_err(|error| {
            TorbenError::new(
                "plugin_manifest_invalid",
                "The plugin manifest is not valid JSON.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let publisher_key = decode_verifying_key(
            &selection.publisher.public_key,
            "plugin_publisher_key_invalid",
            "The plugin publisher key is invalid.",
        )?;
        PluginVerifier::official(publisher_key).verify_manifest(&manifest)?;
        if manifest.id != selection.entry.plugin_id
            || manifest.version != selection.entry.version
            || manifest.publisher != selection.publisher.display_name
        {
            return Err(TorbenError::new(
                "plugin_registry_manifest_mismatch",
                "The plugin manifest identity does not match its signed registry entry.",
            ));
        }
        let target = current_plugin_target(&manifest)?;
        safe_plugin_executable_path(&target.executable)?;
        Ok(manifest)
    }

    /// Verifies a downloaded package against its already selected registry entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest or current-target executable no longer matches the
    /// registry and publisher trust chain.
    pub fn verify_selected_package(
        &self,
        selection: &RegistryPluginSelection,
        manifest_path: &Path,
    ) -> TorbenResult<VerifiedPlugin> {
        let bytes = std::fs::read(manifest_path).map_err(|error| io_error(&error))?;
        self.verify_manifest_bytes(selection, &bytes)?;
        let publisher_key = decode_verifying_key(
            &selection.publisher.public_key,
            "plugin_publisher_key_invalid",
            "The plugin publisher key is invalid.",
        )?;
        PluginVerifier::official(publisher_key).verify(manifest_path)
    }

    fn validate_registry(&self, registry: &PluginRegistry) -> TorbenResult<()> {
        self.verify_root_signature(registry)?;
        if registry.schema_version != PLUGIN_REGISTRY_SCHEMA_VERSION {
            return Err(TorbenError::new(
                "plugin_registry_schema_incompatible",
                "The official plugin registry schema is incompatible with this host.",
            ));
        }
        if registry.sequence == 0 || registry.sequence > MAX_SAFE_REGISTRY_SEQUENCE {
            return Err(TorbenError::new(
                "plugin_registry_sequence_invalid",
                "The official plugin registry sequence is outside the supported safe integer range.",
            ));
        }
        let host_version = ExactVersion::from_str(env!("CARGO_PKG_VERSION"))?;
        if registry.minimum_host_version > host_version {
            return Err(TorbenError::new(
                "plugin_registry_host_incompatible",
                "The official plugin registry requires a newer Torben App host.",
            ));
        }
        ensure_registry_unique(registry)
    }

    fn verify_root_signature(&self, registry: &PluginRegistry) -> TorbenResult<()> {
        let encoded = registry.signature.as_deref().ok_or_else(|| {
            TorbenError::new(
                "plugin_registry_signature_missing",
                "The official plugin registry is unsigned.",
            )
        })?;
        let mut unsigned = registry.clone();
        unsigned.signature = None;
        let payload = serde_json::to_vec(&unsigned).map_err(|error| {
            TorbenError::internal("Could not serialize the plugin registry signature payload.")
                .with_detail("reason", error.to_string())
        })?;
        verify_encoded_signature(
            &self.root_key,
            &payload,
            encoded,
            "plugin_registry_signature_invalid",
            "The official plugin registry signature is invalid.",
        )
    }
}

pub struct PluginClient {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    call_timeout: Duration,
    capabilities: Option<Vec<PluginCapability>>,
}

impl PluginClient {
    /// Starts a verified plugin process with piped JSON-RPC stdio.
    ///
    /// # Errors
    ///
    /// Returns an error when the process or its stdio pipes cannot be created.
    pub fn spawn(plugin: &VerifiedPlugin, call_timeout: Duration) -> TorbenResult<Self> {
        Self::spawn_executable(
            &plugin.executable,
            call_timeout,
            Some(plugin.manifest.capabilities.clone()),
        )
    }

    /// Starts a trusted plugin shipped inside the signed Torben App package.
    ///
    /// This entry point is only for first-party bundled plugins. Registry and sideloaded plugins
    /// must be represented by a [`VerifiedPlugin`] and started with [`Self::spawn`].
    ///
    /// # Errors
    ///
    /// Returns an error when the executable is missing or its process and stdio pipes cannot be
    /// created.
    pub fn spawn_bundled(executable: &Path, call_timeout: Duration) -> TorbenResult<Self> {
        Self::spawn_bundled_internal(executable, call_timeout, None)
    }

    /// Starts a trusted bundled plugin and restricts protocol calls to its declared capabilities.
    ///
    /// # Errors
    ///
    /// Returns an error when the executable is missing or its process and stdio pipes cannot be
    /// created.
    pub fn spawn_bundled_scoped(
        executable: &Path,
        capabilities: &[PluginCapability],
        call_timeout: Duration,
    ) -> TorbenResult<Self> {
        Self::spawn_bundled_internal(executable, call_timeout, Some(capabilities.to_vec()))
    }

    fn spawn_bundled_internal(
        executable: &Path,
        call_timeout: Duration,
        capabilities: Option<Vec<PluginCapability>>,
    ) -> TorbenResult<Self> {
        if !executable.is_file() {
            return Err(TorbenError::new(
                "bundled_plugin_missing",
                "A bundled Torben App plugin executable is missing.",
            )
            .with_detail("path", executable.display().to_string())
            .with_remediation("Reinstall Torben App or rebuild the complete workspace."));
        }
        Self::spawn_executable(executable, call_timeout, capabilities)
    }

    fn spawn_executable(
        executable: &Path,
        call_timeout: Duration,
        capabilities: Option<Vec<PluginCapability>>,
    ) -> TorbenResult<Self> {
        Self::spawn_command(
            Command::new(executable),
            executable,
            call_timeout,
            capabilities,
        )
    }

    fn spawn_command(
        mut command: Command,
        executable: &Path,
        call_timeout: Duration,
        capabilities: Option<Vec<PluginCapability>>,
    ) -> TorbenResult<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                TorbenError::new("plugin_start_failed", "Could not start the plugin process.")
                    .with_detail("path", executable.display().to_string())
                    .with_detail("reason", error.to_string())
            })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            TorbenError::new(
                "plugin_stdio_unavailable",
                "The plugin stdin pipe is unavailable.",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            TorbenError::new(
                "plugin_stdio_unavailable",
                "The plugin stdout pipe is unavailable.",
            )
        })?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            next_id: 1,
            call_timeout,
            capabilities,
        })
    }

    /// Sends one JSON-RPC request and waits for its matching response.
    ///
    /// # Errors
    ///
    /// Returns an error for serialization failures, I/O failures, timeouts, malformed responses,
    /// mismatched request identifiers, or plugin-reported errors.
    pub async fn call<P, R>(&mut self, method: &str, params: &P) -> TorbenResult<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        self.call_internal(method, params, None, None).await
    }

    /// Sends one JSON-RPC request and forwards validated operation progress notifications while
    /// waiting for its matching response.
    ///
    /// # Errors
    ///
    /// Returns an error for the same conditions as [`Self::call`], or when a notification is
    /// unsupported, malformed, belongs to another operation, exceeds protocol bounds, or is
    /// rejected by the event handler.
    pub async fn call_with_operation_events<P, R, F>(
        &mut self,
        method: &str,
        params: &P,
        operation_id: OperationId,
        mut on_event: F,
    ) -> TorbenResult<R>
    where
        P: Serialize,
        R: DeserializeOwned,
        F: FnMut(PluginOperationEvent) -> TorbenResult<()> + Send,
    {
        self.call_internal(method, params, Some(operation_id), Some(&mut on_event))
            .await
    }

    async fn call_internal<P, R>(
        &mut self,
        method_name: &str,
        params: &P,
        operation_id: Option<OperationId>,
        on_event: Option<&mut (dyn FnMut(PluginOperationEvent) -> TorbenResult<()> + Send)>,
    ) -> TorbenResult<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        self.authorize_method(method_name)?;
        let id = self.next_id;
        self.next_id += 1;
        let params =
            serde_json::to_value(params).map_err(|error| protocol_serialize_error(&error))?;
        let request = JsonRpcRequest::new(id, method_name, params);
        let mut payload =
            serde_json::to_vec(&request).map_err(|error| protocol_serialize_error(&error))?;
        payload.push(b'\n');
        timeout(self.call_timeout, self.stdin.write_all(&payload))
            .await
            .map_err(|_| plugin_timeout(method_name))?
            .map_err(|error| io_error(&error))?;
        timeout(self.call_timeout, self.stdin.flush())
            .await
            .map_err(|_| plugin_timeout(method_name))?
            .map_err(|error| io_error(&error))?;
        let response = timeout(
            self.call_timeout,
            self.read_response(operation_id.as_ref(), on_event),
        )
        .await
        .map_err(|_| plugin_timeout(method_name))??;
        if response.jsonrpc != "2.0" || response.id != id {
            return Err(TorbenError::new(
                "plugin_response_mismatch",
                "The plugin response does not match the active request.",
            ));
        }
        if let Some(error) = response.error {
            return Err(error.data.unwrap_or_else(|| {
                TorbenError::new("plugin_error", error.message)
                    .with_detail("rpcCode", error.code.to_string())
            }));
        }
        let result = response.result.ok_or_else(|| {
            TorbenError::new(
                "plugin_result_missing",
                "The plugin response has no result.",
            )
        })?;
        serde_json::from_value(result).map_err(|error| {
            TorbenError::new(
                "plugin_result_invalid",
                "The plugin result has an unexpected shape.",
            )
            .with_detail("reason", error.to_string())
        })
    }

    fn authorize_method(&self, method_name: &str) -> TorbenResult<()> {
        let Some(capabilities) = &self.capabilities else {
            return Ok(());
        };
        let required = required_capabilities(method_name);
        if required.is_empty()
            || required
                .iter()
                .any(|capability| capabilities.contains(capability))
        {
            return Ok(());
        }
        Err(TorbenError::new(
            "plugin_capability_denied",
            "The plugin manifest does not grant the capability required for this method.",
        )
        .with_detail("method", method_name)
        .with_detail(
            "requiredCapabilities",
            required
                .iter()
                .map(capability_name)
                .collect::<Vec<_>>()
                .join(","),
        ))
    }

    async fn read_response(
        &mut self,
        operation_id: Option<&OperationId>,
        mut on_event: Option<&mut (dyn FnMut(PluginOperationEvent) -> TorbenResult<()> + Send)>,
    ) -> TorbenResult<JsonRpcResponse> {
        let mut event_count = 0_usize;
        loop {
            let line = self
                .stdout
                .next_line()
                .await
                .map_err(|error| io_error(&error))?
                .ok_or_else(|| {
                    TorbenError::new(
                        "plugin_exited",
                        "The plugin exited before returning a response.",
                    )
                })?;
            let value: serde_json::Value = serde_json::from_str(&line).map_err(|error| {
                TorbenError::new(
                    "plugin_response_invalid",
                    "The plugin returned malformed JSON.",
                )
                .with_detail("reason", error.to_string())
            })?;
            if value.get("id").is_some() {
                return serde_json::from_value::<JsonRpcResponse>(value).map_err(|error| {
                    TorbenError::new(
                        "plugin_response_invalid",
                        "The plugin returned a malformed JSON-RPC response.",
                    )
                    .with_detail("reason", error.to_string())
                });
            }
            event_count += 1;
            if event_count > MAX_OPERATION_EVENTS_PER_CALL {
                return Err(TorbenError::new(
                    "plugin_notification_limit_exceeded",
                    "The plugin emitted too many notifications for one request.",
                ));
            }
            let notification: JsonRpcNotification =
                serde_json::from_value(value).map_err(|error| {
                    TorbenError::new(
                        "plugin_notification_invalid",
                        "The plugin returned a malformed JSON-RPC notification.",
                    )
                    .with_detail("reason", error.to_string())
                })?;
            let expected_operation = operation_id.ok_or_else(|| {
                TorbenError::new(
                    "plugin_notification_unexpected",
                    "The plugin emitted a notification for a request that does not accept one.",
                )
                .with_detail("method", &notification.method)
            })?;
            let handler = on_event.as_deref_mut().ok_or_else(|| {
                TorbenError::new(
                    "plugin_notification_unexpected",
                    "The plugin emitted a notification without an operation event handler.",
                )
            })?;
            let event = validate_operation_notification(notification, expected_operation)?;
            handler(event)?;
        }
    }

    /// Terminates the plugin child process.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system cannot terminate the process.
    pub async fn shutdown(mut self) -> TorbenResult<()> {
        self.child.kill().await.map_err(|error| io_error(&error))
    }
}

fn validate_operation_notification(
    notification: JsonRpcNotification,
    expected_operation: &OperationId,
) -> TorbenResult<PluginOperationEvent> {
    if notification.jsonrpc != "2.0" || notification.method != method::OPERATION_EVENT {
        return Err(TorbenError::new(
            "plugin_notification_unsupported",
            "The plugin emitted an unsupported JSON-RPC notification.",
        )
        .with_detail("method", notification.method));
    }
    let event: PluginOperationEvent =
        serde_json::from_value(notification.params).map_err(|error| {
            TorbenError::new(
                "plugin_operation_event_invalid",
                "The plugin emitted a malformed operation event.",
            )
            .with_detail("reason", error.to_string())
        })?;
    if &event.operation_id != expected_operation {
        return Err(TorbenError::new(
            "plugin_operation_event_mismatch",
            "The plugin operation event does not match the active request.",
        )
        .with_detail("expectedOperationId", expected_operation.to_string())
        .with_detail("actualOperationId", event.operation_id.to_string()));
    }
    let phase_valid = !event.phase.is_empty()
        && event.phase.len() <= 64
        && event.phase.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        });
    if !phase_valid {
        return Err(TorbenError::new(
            "plugin_operation_event_invalid",
            "The plugin operation event phase is invalid.",
        ));
    }
    if event.message.trim().is_empty()
        || event.message.len() > 1_024
        || event.message.chars().any(char::is_control)
    {
        return Err(TorbenError::new(
            "plugin_operation_event_invalid",
            "The plugin operation event message is invalid.",
        ));
    }
    if event
        .progress
        .is_some_and(|progress| !progress.is_finite() || !(0.0..=1.0).contains(&progress))
    {
        return Err(TorbenError::new(
            "plugin_operation_event_invalid",
            "The plugin operation event progress must be between zero and one.",
        ));
    }
    Ok(event)
}

fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn current_plugin_target(manifest: &PluginManifest) -> TorbenResult<&PluginTarget> {
    let target = current_target();
    manifest
        .targets
        .iter()
        .find(|candidate| candidate.target == target)
        .ok_or_else(|| {
            TorbenError::new(
                "plugin_target_missing",
                "The plugin does not support this platform target.",
            )
            .with_detail("target", target)
        })
}

fn safe_plugin_executable_path(value: &str) -> TorbenResult<PathBuf> {
    safe_relative_path(value, "plugin_executable_path_unsafe").map_err(|error| {
        TorbenError::new(
            error.code,
            "The plugin executable must be a safe relative path.",
        )
    })
}

fn ensure_registry_unique(registry: &PluginRegistry) -> TorbenResult<()> {
    let mut publishers = BTreeSet::new();
    for publisher in &registry.publishers {
        if publisher.id.trim().is_empty() || !publishers.insert(publisher.id.as_str()) {
            return Err(TorbenError::new(
                "plugin_registry_ambiguous",
                "The official plugin registry contains duplicate or empty publisher identifiers.",
            ));
        }
    }
    let mut entries = BTreeSet::new();
    let mut package_directories = BTreeSet::new();
    for entry in &registry.entries {
        let identity = (entry.plugin_id.to_string(), entry.version.to_string());
        if !entries.insert(identity) {
            return Err(TorbenError::new(
                "plugin_registry_ambiguous",
                "The official plugin registry contains duplicate plugin versions.",
            ));
        }
        let manifest_path =
            safe_relative_path(&entry.manifest_path, "plugin_manifest_path_unsafe")?;
        let package_directory = manifest_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty() && path != &Path::new("."));
        let package_directory = package_directory.ok_or_else(|| {
            TorbenError::new(
                "plugin_registry_package_layout_invalid",
                "Every registry manifest must use a version-specific package directory.",
            )
        })?;
        if !package_directories.insert(package_directory.to_string_lossy().to_ascii_lowercase()) {
            return Err(TorbenError::new(
                "plugin_registry_ambiguous",
                "The official plugin registry reuses a package directory.",
            ));
        }
    }
    Ok(())
}

fn safe_relative_path(value: &str, code: &str) -> TorbenResult<PathBuf> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > MAX_PORTABLE_PATH_LENGTH
        || value.contains('\\')
        || path.is_absolute()
        || value
            .split('/')
            .any(|component| !portable_path_component(component))
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(TorbenError::new(
            code,
            "The registry path must be a safe relative path.",
        ));
    }
    Ok(path.to_path_buf())
}

fn portable_path_component(component: &str) -> bool {
    if component.is_empty()
        || component.len() > MAX_PORTABLE_COMPONENT_LENGTH
        || component.starts_with([' ', '.'])
        || component.ends_with([' ', '.'])
        || !component.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+' | b' ')
        })
    {
        return false;
    }
    let base = component
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) {
        return false;
    }
    let numbered_device = |prefix: &str| {
        base.strip_prefix(prefix)
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
    };
    !numbered_device("COM") && !numbered_device("LPT")
}

fn decode_verifying_key(encoded: &str, code: &str, message: &str) -> TorbenResult<VerifyingKey> {
    let bytes = STANDARD.decode(encoded).map_err(|error| {
        TorbenError::new(code, message).with_detail("reason", error.to_string())
    })?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        TorbenError::new(code, message).with_detail("length", bytes.len().to_string())
    })?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|error| TorbenError::new(code, message).with_detail("reason", error.to_string()))
}

fn verify_encoded_signature(
    key: &VerifyingKey,
    payload: &[u8],
    encoded: &str,
    code: &str,
    message: &str,
) -> TorbenResult<()> {
    let bytes = STANDARD.decode(encoded).map_err(|error| {
        TorbenError::new(code, message).with_detail("reason", error.to_string())
    })?;
    let signature = Signature::try_from(bytes.as_slice()).map_err(|error| {
        TorbenError::new(code, message).with_detail("reason", error.to_string())
    })?;
    key.verify_strict(payload, &signature)
        .map_err(|error| TorbenError::new(code, message).with_detail("reason", error.to_string()))
}

fn sha256_file(path: &Path) -> TorbenResult<String> {
    let bytes = std::fs::read(path).map_err(|error| io_error(&error))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn io_error(error: &std::io::Error) -> TorbenError {
    TorbenError::new("plugin_io_failed", "Plugin process I/O failed.")
        .with_detail("reason", error.to_string())
}

fn protocol_serialize_error(error: &serde_json::Error) -> TorbenError {
    TorbenError::new(
        "plugin_request_invalid",
        "Could not serialize a plugin request.",
    )
    .with_detail("reason", error.to_string())
}

fn plugin_timeout(method: &str) -> TorbenError {
    TorbenError::new(
        "plugin_timeout",
        "The plugin did not respond before the timeout.",
    )
    .with_detail("method", method)
}

fn required_capabilities(method_name: &str) -> &'static [PluginCapability] {
    match method_name {
        method::VERSIONS_LIST | method::VERSION_RESOLVE => &[PluginCapability::VersionDiscovery],
        method::EXTERNAL_DISCOVER => &[PluginCapability::ExternalDiscovery],
        method::INSTALL_PLAN => &[PluginCapability::ManagedInstall],
        method::HEALTH_CHECK => &[
            PluginCapability::ManagedInstall,
            PluginCapability::GlobalSelection,
            PluginCapability::ManagedUninstall,
        ],
        method::UNINSTALL_PLAN => &[PluginCapability::ManagedUninstall],
        method::SCHEMA_PAGES | method::SCHEMA_ACTION => &[PluginCapability::SchemaUi],
        _ => &[],
    }
}

const fn capability_name(capability: &PluginCapability) -> &'static str {
    match capability {
        PluginCapability::VersionDiscovery => "version_discovery",
        PluginCapability::ExternalDiscovery => "external_discovery",
        PluginCapability::ManagedInstall => "managed_install",
        PluginCapability::GlobalSelection => "global_selection",
        PluginCapability::ManagedUninstall => "managed_uninstall",
        PluginCapability::SchemaUi => "schema_ui",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        str::FromStr,
        time::Duration,
    };

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer as _, SigningKey};
    use serde_json::{Value, json};
    use sha2::Digest;
    use tempfile::{TempDir, tempdir};
    #[cfg(unix)]
    use tokio::process::Command;
    use torben_contracts::{
        ExactVersion, OperationId, PluginId,
        plugin::{
            PLUGIN_PROTOCOL_VERSION, PLUGIN_REGISTRY_SCHEMA_VERSION, PluginCapability,
            PluginManifest, PluginPermissions, PluginRegistry, PluginRegistryEntry,
            PluginRegistryPublisher, PluginTarget, method,
        },
    };

    use super::{
        PluginClient, PluginVerifier, RegistryVerifier, current_target, ensure_registry_unique,
        safe_relative_path,
    };

    #[derive(Clone, Copy)]
    enum FixtureBehavior {
        Success,
        Progress,
        UnexpectedProgress,
        TooManyProgressEvents,
        WrongOperation,
        Timeout,
        Exit,
        Malformed,
        MismatchedId,
    }

    #[tokio::test]
    async fn bundled_plugin_returns_a_typed_result() {
        let (_directory, executable) = fixture_plugin(FixtureBehavior::Success);
        let mut client = spawn_fixture(&executable, Duration::from_secs(2));

        let result: Value = client.call("fixture.success", &json!({})).await.unwrap();

        assert_eq!(result["value"], "ok");
    }

    #[tokio::test]
    async fn scoped_plugin_denies_methods_without_the_declared_capability() {
        let (_directory, executable) = fixture_plugin(FixtureBehavior::Success);
        let mut client = spawn_fixture_scoped(
            &executable,
            &[PluginCapability::VersionDiscovery],
            Duration::from_secs(2),
        );

        let denied = client
            .call::<_, Value>(method::SCHEMA_PAGES, &json!({}))
            .await
            .unwrap_err();
        assert_eq!(denied.code, "plugin_capability_denied");
        assert_eq!(
            denied
                .details
                .get("requiredCapabilities")
                .map(String::as_str),
            Some("schema_ui")
        );

        let allowed: Value = client
            .call(method::VERSIONS_LIST, &json!({}))
            .await
            .unwrap();
        assert_eq!(allowed["value"], "ok");
    }

    #[tokio::test]
    async fn bundled_plugin_forwards_only_matching_bounded_operation_events() {
        let operation_id = OperationId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
        let (_directory, executable) = fixture_plugin(FixtureBehavior::Progress);
        let mut client = spawn_fixture(&executable, Duration::from_secs(2));
        let mut events = Vec::new();

        let result: Value = client
            .call_with_operation_events("fixture.progress", &json!({}), operation_id, |event| {
                events.push(event);
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(result["value"], "ok");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].phase, "resolve");
        assert_eq!(events[0].message, "Resolving fixture");
        assert_eq!(events[0].progress, Some(0.5));
    }

    #[tokio::test]
    async fn bundled_plugin_rejects_notifications_for_plain_calls() {
        let (_directory, executable) = fixture_plugin(FixtureBehavior::UnexpectedProgress);
        let mut client = spawn_fixture(&executable, Duration::from_secs(2));

        let error = client
            .call::<_, Value>("fixture.success", &json!({}))
            .await
            .unwrap_err();

        assert_eq!(error.code, "plugin_notification_unexpected");
        assert_eq!(
            error.details.get("method").map(String::as_str),
            Some("operation.event")
        );
    }

    #[tokio::test]
    async fn bundled_plugin_rejects_an_operation_event_for_another_request() {
        let operation_id = OperationId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
        let (_directory, executable) = fixture_plugin(FixtureBehavior::WrongOperation);
        let mut client = spawn_fixture(&executable, Duration::from_secs(2));

        let error = client
            .call_with_operation_events::<_, Value, _>(
                "fixture.progress",
                &json!({}),
                operation_id,
                |_| Ok(()),
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "plugin_operation_event_mismatch");
    }

    #[tokio::test]
    async fn bundled_plugin_limits_operation_events_per_call() {
        let operation_id = OperationId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
        let (_directory, executable) = fixture_plugin(FixtureBehavior::TooManyProgressEvents);
        let mut client = spawn_fixture(&executable, Duration::from_secs(5));

        let error = client
            .call_with_operation_events::<_, Value, _>(
                "fixture.progress",
                &json!({}),
                operation_id,
                |_| Ok(()),
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "plugin_notification_limit_exceeded");
    }

    #[test]
    fn operation_event_validation_rejects_invalid_phase() {
        let operation_id = OperationId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
        let notification = torben_contracts::plugin::JsonRpcNotification {
            jsonrpc: "2.0".to_owned(),
            method: torben_contracts::plugin::method::OPERATION_EVENT.to_owned(),
            params: json!({
                "operationId": operation_id,
                "phase": "Invalid Phase",
                "message": "Resolving fixture",
                "progress": 0.5
            }),
        };

        let error =
            super::validate_operation_notification(notification, &operation_id).unwrap_err();

        assert_eq!(error.code, "plugin_operation_event_invalid");
    }

    #[test]
    fn operation_event_validation_rejects_unknown_notification_method() {
        let operation_id = OperationId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
        let notification = torben_contracts::plugin::JsonRpcNotification {
            jsonrpc: "2.0".to_owned(),
            method: "operation.unknown".to_owned(),
            params: json!({
                "operationId": operation_id,
                "phase": "resolve",
                "message": "Resolving fixture",
                "progress": 0.5
            }),
        };

        let error =
            super::validate_operation_notification(notification, &operation_id).unwrap_err();

        assert_eq!(error.code, "plugin_notification_unsupported");
    }

    #[tokio::test]
    async fn bundled_plugin_timeout_is_isolated() {
        let (_directory, executable) = fixture_plugin(FixtureBehavior::Timeout);
        let mut client = spawn_fixture(&executable, Duration::from_millis(50));

        let error = client
            .call::<_, Value>("fixture.timeout", &json!({}))
            .await
            .unwrap_err();

        assert_eq!(error.code, "plugin_timeout");
        assert_eq!(
            error.details.get("method").map(String::as_str),
            Some("fixture.timeout")
        );
    }

    #[tokio::test]
    async fn bundled_plugin_exit_is_isolated() {
        let (_directory, executable) = fixture_plugin(FixtureBehavior::Exit);
        let mut client = spawn_fixture(&executable, Duration::from_secs(2));

        let error = client
            .call::<_, Value>("fixture.exit", &json!({}))
            .await
            .unwrap_err();

        assert_eq!(error.code, "plugin_exited");
    }

    #[tokio::test]
    async fn malformed_plugin_response_is_isolated() {
        let (_directory, executable) = fixture_plugin(FixtureBehavior::Malformed);
        let mut client = spawn_fixture(&executable, Duration::from_secs(2));

        let error = client
            .call::<_, Value>("fixture.malformed", &json!({}))
            .await
            .unwrap_err();

        assert_eq!(error.code, "plugin_response_invalid");
    }

    #[tokio::test]
    async fn mismatched_plugin_response_is_isolated() {
        let (_directory, executable) = fixture_plugin(FixtureBehavior::MismatchedId);
        let mut client = spawn_fixture(&executable, Duration::from_secs(2));

        let error = client
            .call::<_, Value>("fixture.mismatch", &json!({}))
            .await
            .unwrap_err();

        assert_eq!(error.code, "plugin_response_mismatch");
    }

    #[test]
    fn developer_mode_still_checks_executable_hash() {
        let directory = tempdir().unwrap();
        let executable_name = if cfg!(windows) {
            "plugin.exe"
        } else {
            "plugin"
        };
        fs::write(directory.path().join(executable_name), b"test plugin").unwrap();
        let mut manifest = PluginManifest {
            id: PluginId::new("app.torben.plugin.test").unwrap(),
            display_name: "Test".to_owned(),
            version: ExactVersion::from_str("0.1.0").unwrap(),
            protocol_version: PLUGIN_PROTOCOL_VERSION,
            minimum_host_version: ExactVersion::from_str("0.1.0").unwrap(),
            publisher: "test".to_owned(),
            capabilities: Vec::new(),
            permissions: PluginPermissions::default(),
            targets: vec![PluginTarget {
                target: current_target(),
                executable: executable_name.to_owned(),
                sha256: hex::encode(sha2::Sha256::digest(b"test plugin")),
            }],
            signature: None,
            revoked: false,
        };
        let manifest_path = directory.path().join("plugin.json");
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(
            PluginVerifier::developer_mode()
                .verify(&manifest_path)
                .is_ok()
        );

        manifest.minimum_host_version = ExactVersion::from_str("999.0.0").unwrap();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error = PluginVerifier::developer_mode()
            .verify(&manifest_path)
            .unwrap_err();
        assert_eq!(error.code, "plugin_host_version_incompatible");
    }

    #[test]
    fn manifest_rejects_unsafe_and_duplicate_permission_declarations() {
        let directory = tempdir().unwrap();
        let executable_name = if cfg!(windows) {
            "plugin.exe"
        } else {
            "plugin"
        };
        fs::write(directory.path().join(executable_name), b"test plugin").unwrap();
        let manifest_path = directory.path().join("plugin.json");
        let base = PluginManifest {
            id: PluginId::new("app.torben.plugin.permissions").unwrap(),
            display_name: "Permissions".to_owned(),
            version: ExactVersion::from_str("0.1.0").unwrap(),
            protocol_version: PLUGIN_PROTOCOL_VERSION,
            minimum_host_version: ExactVersion::from_str("0.1.0").unwrap(),
            publisher: "test".to_owned(),
            capabilities: vec![PluginCapability::SchemaUi],
            permissions: PluginPermissions::default(),
            targets: vec![PluginTarget {
                target: current_target(),
                executable: executable_name.to_owned(),
                sha256: hex::encode(sha2::Sha256::digest(b"test plugin")),
            }],
            signature: None,
            revoked: false,
        };
        let mut valid = base.clone();
        valid.permissions = PluginPermissions {
            network_domains: vec!["api.example.com".to_owned()],
            filesystem_roots: vec!["staging".to_owned()],
            external_commands: vec!["example-tool".to_owned()],
            package_managers: vec!["winget".to_owned()],
        };
        fs::write(&manifest_path, serde_json::to_vec(&valid).unwrap()).unwrap();
        PluginVerifier::developer_mode()
            .verify(&manifest_path)
            .unwrap();

        let cases = [
            PluginPermissions {
                network_domains: vec!["https://example.com/path".to_owned()],
                ..PluginPermissions::default()
            },
            PluginPermissions {
                filesystem_roots: vec!["../user-home".to_owned()],
                ..PluginPermissions::default()
            },
            PluginPermissions {
                external_commands: vec!["../tool".to_owned()],
                ..PluginPermissions::default()
            },
            PluginPermissions {
                package_managers: vec!["unknown-manager".to_owned()],
                ..PluginPermissions::default()
            },
            PluginPermissions {
                network_domains: vec!["example.com".to_owned(), "example.com".to_owned()],
                ..PluginPermissions::default()
            },
        ];

        for permissions in cases {
            let mut manifest = base.clone();
            manifest.permissions = permissions;
            fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            let error = PluginVerifier::developer_mode()
                .verify(&manifest_path)
                .unwrap_err();
            assert!(
                matches!(
                    error.code.as_str(),
                    "plugin_permission_invalid" | "plugin_permission_duplicate"
                ),
                "unexpected permission validation error: {}",
                error.code
            );
        }

        let mut duplicate_capability = base;
        duplicate_capability.capabilities =
            vec![PluginCapability::SchemaUi, PluginCapability::SchemaUi];
        fs::write(
            &manifest_path,
            serde_json::to_vec(&duplicate_capability).unwrap(),
        )
        .unwrap();
        let error = PluginVerifier::developer_mode()
            .verify(&manifest_path)
            .unwrap_err();
        assert_eq!(error.code, "plugin_capability_duplicate");
    }

    #[test]
    fn signed_registry_authorizes_a_publisher_and_exact_plugin_asset() {
        let fixture = registry_fixture();
        let verified = RegistryVerifier::new(fixture.root.verifying_key())
            .verify(&fixture.path, &fixture.plugin_id, None)
            .unwrap();

        assert_eq!(verified.plugin.manifest.id, fixture.plugin_id);
        assert_eq!(verified.publisher_id, "example.publisher");
        assert_eq!(verified.manifest_path, fixture.manifest_path);
    }

    #[test]
    fn publisher_manifest_is_verified_before_the_executable_download() {
        let fixture = registry_fixture();
        let verifier = RegistryVerifier::new(fixture.root.verifying_key());
        let registry_bytes = fs::read(&fixture.path).unwrap();
        let registry = verifier.verify_registry_bytes(&registry_bytes).unwrap();
        let selection = verifier
            .select_plugin(&registry, &fixture.plugin_id, None)
            .unwrap();
        let manifest_bytes = fs::read(&fixture.manifest_path).unwrap();

        let manifest = verifier
            .verify_manifest_bytes(&selection, &manifest_bytes)
            .unwrap();

        assert_eq!(manifest.id, fixture.plugin_id);
        assert_eq!(manifest.version, selection.entry.version);
    }

    #[test]
    fn modified_registry_is_rejected_before_entry_selection() {
        let mut fixture = registry_fixture();
        fixture.registry.generated_at = "tampered".to_owned();
        write_registry(&fixture.path, &fixture.registry);

        let error = RegistryVerifier::new(fixture.root.verifying_key())
            .verify(&fixture.path, &fixture.plugin_id, None)
            .unwrap_err();

        assert_eq!(error.code, "plugin_registry_signature_invalid");
    }

    #[test]
    fn signed_registry_revocation_blocks_a_previously_valid_plugin() {
        let mut fixture = registry_fixture();
        fixture.registry.entries[0].revoked = true;
        sign_registry(&mut fixture.registry, &fixture.root);
        write_registry(&fixture.path, &fixture.registry);

        let error = RegistryVerifier::new(fixture.root.verifying_key())
            .verify(&fixture.path, &fixture.plugin_id, None)
            .unwrap_err();

        assert_eq!(error.code, "plugin_registry_entry_revoked");
    }

    #[test]
    fn signed_publisher_revocation_blocks_its_plugins() {
        let mut fixture = registry_fixture();
        fixture.registry.publishers[0].revoked = true;
        sign_registry(&mut fixture.registry, &fixture.root);
        write_registry(&fixture.path, &fixture.registry);

        let error = RegistryVerifier::new(fixture.root.verifying_key())
            .verify(&fixture.path, &fixture.plugin_id, None)
            .unwrap_err();

        assert_eq!(error.code, "plugin_registry_publisher_revoked");
    }

    #[test]
    fn registry_requiring_a_newer_host_is_rejected() {
        let mut fixture = registry_fixture();
        fixture.registry.minimum_host_version = ExactVersion::from_str("999.0.0").unwrap();
        sign_registry(&mut fixture.registry, &fixture.root);
        write_registry(&fixture.path, &fixture.registry);

        let error = RegistryVerifier::new(fixture.root.verifying_key())
            .verify(&fixture.path, &fixture.plugin_id, None)
            .unwrap_err();

        assert_eq!(error.code, "plugin_registry_host_incompatible");
    }

    #[test]
    fn registry_sequence_must_advance_from_a_nonzero_value() {
        let mut fixture = registry_fixture();
        fixture.registry.sequence = 0;
        sign_registry(&mut fixture.registry, &fixture.root);
        write_registry(&fixture.path, &fixture.registry);

        let error = RegistryVerifier::new(fixture.root.verifying_key())
            .verify(&fixture.path, &fixture.plugin_id, None)
            .unwrap_err();

        assert_eq!(error.code, "plugin_registry_sequence_invalid");
    }

    #[test]
    fn signed_registry_cannot_reference_a_manifest_outside_its_root() {
        let mut fixture = registry_fixture();
        fixture.registry.entries[0].manifest_path = "../plugin.json".to_owned();
        sign_registry(&mut fixture.registry, &fixture.root);
        write_registry(&fixture.path, &fixture.registry);

        let error = RegistryVerifier::new(fixture.root.verifying_key())
            .verify(&fixture.path, &fixture.plugin_id, None)
            .unwrap_err();

        assert_eq!(error.code, "plugin_manifest_path_unsafe");
    }

    #[test]
    fn registry_paths_reject_cross_platform_filesystem_aliases() {
        assert!(
            safe_relative_path(
                "packages/example/1.2.3+build/plugin.json",
                "plugin_manifest_path_unsafe"
            )
            .is_ok()
        );
        for path in [
            "packages//plugin.json",
            "packages/CON/plugin.json",
            "packages/com1.txt/plugin.json",
            "packages/example./plugin.json",
            "packages/example /plugin.json",
            "packages/example/plugin.json:payload",
            "packages/插件/plugin.json",
        ] {
            let error = safe_relative_path(path, "plugin_manifest_path_unsafe").unwrap_err();
            assert_eq!(error.code, "plugin_manifest_path_unsafe", "path: {path}");
        }
    }

    #[test]
    fn registry_package_directories_are_unique_across_windows_case_folding() {
        let mut fixture = registry_fixture();
        let mut duplicate = fixture.registry.entries[0].clone();
        duplicate.plugin_id = PluginId::new("app.torben.plugin.other").unwrap();
        duplicate.manifest_path = "PACKAGES/TEST/plugin.json".to_owned();
        fixture.registry.entries.push(duplicate);

        let error = ensure_registry_unique(&fixture.registry).unwrap_err();

        assert_eq!(error.code, "plugin_registry_ambiguous");
    }

    #[test]
    fn manifest_targets_cannot_reuse_case_folded_executable_paths() {
        let fixture = registry_fixture();
        let mut manifest: PluginManifest =
            serde_json::from_slice(&fs::read(&fixture.manifest_path).unwrap()).unwrap();
        let mut duplicate = manifest.targets[0].clone();
        duplicate.target = "another-target".to_owned();
        duplicate.executable = duplicate.executable.to_ascii_uppercase();
        manifest.targets.push(duplicate);
        fs::write(
            &fixture.manifest_path,
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let error = PluginVerifier::developer_mode()
            .verify(&fixture.manifest_path)
            .unwrap_err();

        assert_eq!(error.code, "plugin_target_ambiguous");
    }

    #[test]
    fn executable_changed_after_manifest_signing_fails_the_platform_hash() {
        let fixture = registry_fixture();
        let executable = fixture.manifest_path.with_file_name(if cfg!(windows) {
            "plugin.exe"
        } else {
            "plugin"
        });
        fs::write(executable, b"tampered executable").unwrap();

        let error = RegistryVerifier::new(fixture.root.verifying_key())
            .verify(&fixture.path, &fixture.plugin_id, None)
            .unwrap_err();

        assert_eq!(error.code, "plugin_hash_mismatch");
    }

    #[test]
    fn manifest_changed_after_registry_signing_fails_its_pinned_hash() {
        let fixture = registry_fixture();
        fs::write(&fixture.manifest_path, b"{}").unwrap();

        let error = RegistryVerifier::new(fixture.root.verifying_key())
            .verify(&fixture.path, &fixture.plugin_id, None)
            .unwrap_err();

        assert_eq!(error.code, "plugin_registry_manifest_hash_mismatch");
    }

    struct RegistryFixture {
        _directory: TempDir,
        root: SigningKey,
        path: PathBuf,
        manifest_path: PathBuf,
        plugin_id: PluginId,
        registry: PluginRegistry,
    }

    fn registry_fixture() -> RegistryFixture {
        let directory = tempdir().unwrap();
        let root = SigningKey::from_bytes(&[7; 32]);
        let publisher = SigningKey::from_bytes(&[9; 32]);
        let package = directory.path().join("packages").join("test");
        fs::create_dir_all(&package).unwrap();
        let executable_name = if cfg!(windows) {
            "plugin.exe"
        } else {
            "plugin"
        };
        fs::write(package.join(executable_name), b"official plugin fixture").unwrap();
        let plugin_id = PluginId::new("app.torben.plugin.test").unwrap();
        let mut manifest = PluginManifest {
            id: plugin_id.clone(),
            display_name: "Test".to_owned(),
            version: ExactVersion::from_str("0.1.0").unwrap(),
            protocol_version: PLUGIN_PROTOCOL_VERSION,
            minimum_host_version: ExactVersion::from_str("0.1.0").unwrap(),
            publisher: "Example Publisher".to_owned(),
            capabilities: Vec::new(),
            permissions: PluginPermissions::default(),
            targets: vec![PluginTarget {
                target: current_target(),
                executable: executable_name.to_owned(),
                sha256: hex::encode(sha2::Sha256::digest(b"official plugin fixture")),
            }],
            signature: None,
            revoked: false,
        };
        let payload = serde_json::to_vec(&manifest).unwrap();
        manifest.signature = Some(STANDARD.encode(publisher.sign(&payload).to_bytes()));
        let manifest_path = package.join("plugin.json");
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(&manifest_path, &manifest_bytes).unwrap();
        let mut registry = PluginRegistry {
            schema_version: PLUGIN_REGISTRY_SCHEMA_VERSION,
            sequence: 1,
            generated_at: "2026-08-23T00:00:00Z".to_owned(),
            minimum_host_version: ExactVersion::from_str("0.1.0").unwrap(),
            publishers: vec![PluginRegistryPublisher {
                id: "example.publisher".to_owned(),
                display_name: "Example Publisher".to_owned(),
                public_key: STANDARD.encode(publisher.verifying_key().to_bytes()),
                revoked: false,
            }],
            entries: vec![PluginRegistryEntry {
                plugin_id: plugin_id.clone(),
                version: ExactVersion::from_str("0.1.0").unwrap(),
                publisher_id: "example.publisher".to_owned(),
                manifest_path: "packages/test/plugin.json".to_owned(),
                manifest_sha256: hex::encode(sha2::Sha256::digest(&manifest_bytes)),
                revoked: false,
            }],
            signature: None,
        };
        sign_registry(&mut registry, &root);
        let path = directory.path().join("registry.json");
        write_registry(&path, &registry);
        RegistryFixture {
            _directory: directory,
            root,
            path,
            manifest_path,
            plugin_id,
            registry,
        }
    }

    fn sign_registry(registry: &mut PluginRegistry, key: &SigningKey) {
        registry.signature = None;
        let payload = serde_json::to_vec(&*registry).unwrap();
        registry.signature = Some(STANDARD.encode(key.sign(&payload).to_bytes()));
    }

    fn write_registry(path: &std::path::Path, registry: &PluginRegistry) {
        fs::write(path, serde_json::to_vec_pretty(registry).unwrap()).unwrap();
    }

    fn fixture_plugin(behavior: FixtureBehavior) -> (TempDir, PathBuf) {
        let directory = tempdir().unwrap();
        let executable = directory.path().join(if cfg!(windows) {
            "fixture-plugin.cmd"
        } else {
            "fixture-plugin"
        });
        let staging = directory.path().join("fixture-plugin.next");
        fs::write(&staging, fixture_script(behavior)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&staging).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&staging, permissions).unwrap();
        }
        fs::rename(staging, &executable).unwrap();
        (directory, executable)
    }

    fn spawn_fixture(executable: &Path, call_timeout: Duration) -> PluginClient {
        spawn_fixture_internal(executable, call_timeout, None)
    }

    fn spawn_fixture_scoped(
        executable: &Path,
        capabilities: &[PluginCapability],
        call_timeout: Duration,
    ) -> PluginClient {
        spawn_fixture_internal(executable, call_timeout, Some(capabilities.to_vec()))
    }

    fn spawn_fixture_internal(
        executable: &Path,
        call_timeout: Duration,
        capabilities: Option<Vec<PluginCapability>>,
    ) -> PluginClient {
        #[cfg(windows)]
        {
            PluginClient::spawn_bundled_internal(executable, call_timeout, capabilities).unwrap()
        }
        #[cfg(unix)]
        {
            let mut command = Command::new("/bin/sh");
            command.arg(executable);
            PluginClient::spawn_command(command, executable, call_timeout, capabilities).unwrap()
        }
    }

    fn fixture_script(behavior: FixtureBehavior) -> &'static str {
        #[cfg(windows)]
        {
            match behavior {
                FixtureBehavior::Success => {
                    "@echo off\r\nset /p request=\r\necho {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"value\":\"ok\"}}\r\n"
                }
                FixtureBehavior::Progress => {
                    "@echo off\r\nset /p request=\r\necho {\"jsonrpc\":\"2.0\",\"method\":\"operation.event\",\"params\":{\"operationId\":\"11111111-1111-4111-8111-111111111111\",\"phase\":\"resolve\",\"message\":\"Resolving fixture\",\"progress\":0.5}}\r\necho {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"value\":\"ok\"}}\r\n"
                }
                FixtureBehavior::UnexpectedProgress => {
                    "@echo off\r\nset /p request=\r\necho {\"jsonrpc\":\"2.0\",\"method\":\"operation.event\",\"params\":{\"operationId\":\"11111111-1111-4111-8111-111111111111\",\"phase\":\"resolve\",\"message\":\"Resolving fixture\",\"progress\":0.5}}\r\n"
                }
                FixtureBehavior::TooManyProgressEvents => {
                    "@echo off\r\nset /p request=\r\nfor /L %%i in (1,1,1025) do echo {\"jsonrpc\":\"2.0\",\"method\":\"operation.event\",\"params\":{\"operationId\":\"11111111-1111-4111-8111-111111111111\",\"phase\":\"resolve\",\"message\":\"Resolving fixture\",\"progress\":0.5}}\r\n"
                }
                FixtureBehavior::WrongOperation => {
                    "@echo off\r\nset /p request=\r\necho {\"jsonrpc\":\"2.0\",\"method\":\"operation.event\",\"params\":{\"operationId\":\"22222222-2222-4222-8222-222222222222\",\"phase\":\"resolve\",\"message\":\"Resolving fixture\",\"progress\":0.5}}\r\n"
                }
                FixtureBehavior::Timeout => {
                    "@echo off\r\nset /p request=\r\nping.exe -n 3 127.0.0.1 >nul\r\n"
                }
                FixtureBehavior::Exit => "@echo off\r\nset /p request=\r\nexit /b 0\r\n",
                FixtureBehavior::Malformed => "@echo off\r\nset /p request=\r\necho not-json\r\n",
                FixtureBehavior::MismatchedId => {
                    "@echo off\r\nset /p request=\r\necho {\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{}}\r\n"
                }
            }
        }
        #[cfg(unix)]
        {
            match behavior {
                FixtureBehavior::Success => {
                    "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"value\":\"ok\"}}'\n"
                }
                FixtureBehavior::Progress => {
                    "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"operation.event\",\"params\":{\"operationId\":\"11111111-1111-4111-8111-111111111111\",\"phase\":\"resolve\",\"message\":\"Resolving fixture\",\"progress\":0.5}}'\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"value\":\"ok\"}}'\n"
                }
                FixtureBehavior::UnexpectedProgress => {
                    "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"operation.event\",\"params\":{\"operationId\":\"11111111-1111-4111-8111-111111111111\",\"phase\":\"resolve\",\"message\":\"Resolving fixture\",\"progress\":0.5}}'\n"
                }
                FixtureBehavior::TooManyProgressEvents => {
                    "#!/bin/sh\nIFS= read -r request\ni=0\nwhile [ \"$i\" -lt 1025 ]; do\n  printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"operation.event\",\"params\":{\"operationId\":\"11111111-1111-4111-8111-111111111111\",\"phase\":\"resolve\",\"message\":\"Resolving fixture\",\"progress\":0.5}}'\n  i=$((i + 1))\ndone\n"
                }
                FixtureBehavior::WrongOperation => {
                    "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"operation.event\",\"params\":{\"operationId\":\"22222222-2222-4222-8222-222222222222\",\"phase\":\"resolve\",\"message\":\"Resolving fixture\",\"progress\":0.5}}'\n"
                }
                FixtureBehavior::Timeout => "#!/bin/sh\nIFS= read -r request\nsleep 2\n",
                FixtureBehavior::Exit => "#!/bin/sh\nIFS= read -r request\nexit 0\n",
                FixtureBehavior::Malformed => {
                    "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' 'not-json'\n"
                }
                FixtureBehavior::MismatchedId => {
                    "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{}}'\n"
                }
            }
        }
    }
}

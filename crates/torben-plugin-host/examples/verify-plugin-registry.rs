use std::{path::Path, process::ExitCode};

use torben_plugin_host::RegistryVerifier;

fn main() -> ExitCode {
    match run() {
        Ok(sequence) => {
            println!("Rust host verified plugin registry sequence {sequence}.");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u64, String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let [registry_path, root_public_key] = arguments.as_slice() else {
        return Err(
            "Usage: verify-plugin-registry <registry.json> <root-public-key-base64>".to_owned(),
        );
    };
    let bytes = std::fs::read(Path::new(registry_path))
        .map_err(|error| format!("Could not read plugin registry: {error}"))?;
    let verifier = RegistryVerifier::from_base64(root_public_key)
        .map_err(|error| format!("{}: {}", error.code, error.message))?;
    let registry = verifier
        .verify_registry_bytes(&bytes)
        .map_err(|error| format!("{}: {}", error.code, error.message))?;
    for entry in &registry.entries {
        let publisher = registry
            .publishers
            .iter()
            .find(|publisher| publisher.id == entry.publisher_id)
            .ok_or_else(|| {
                format!(
                    "Registry entry {}@{} has no publisher.",
                    entry.plugin_id, entry.version
                )
            })?;
        if entry.revoked || publisher.revoked {
            continue;
        }
        verifier
            .verify(
                Path::new(registry_path),
                &entry.plugin_id,
                Some(&entry.version),
            )
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
    }
    Ok(registry.sequence)
}

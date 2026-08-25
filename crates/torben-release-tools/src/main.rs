use std::{io::Read as _, path::Path, process::ExitCode};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use minisign_verify::{PublicKey, Signature};

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match run(&arguments) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: &[String]) -> Result<String, String> {
    match arguments {
        [command, public_key] if command == "validate-updater-key" => {
            parse_public_key(public_key)?;
            Ok("Validated updater public key.".to_owned())
        }
        [command, artifact, signature, public_key] if command == "verify-updater" => {
            verify_updater_signature(Path::new(artifact), Path::new(signature), public_key)?;
            Ok(format!("Verified updater signature for {artifact}."))
        }
        [command, ..] => Err(format!(
            "Unknown or malformed release tool command: {command}"
        )),
        [] => {
            Err("Usage: torben-release-tools <validate-updater-key|verify-updater> ...".to_owned())
        }
    }
}

fn verify_updater_signature(
    artifact: &Path,
    signature_path: &Path,
    encoded_public_key: &str,
) -> Result<(), String> {
    let artifact_metadata = std::fs::symlink_metadata(artifact)
        .map_err(|error| format!("Could not read updater artifact metadata: {error}"))?;
    let signature_metadata = std::fs::symlink_metadata(signature_path)
        .map_err(|error| format!("Could not read updater signature metadata: {error}"))?;
    if !artifact_metadata.is_file()
        || artifact_metadata.file_type().is_symlink()
        || !signature_metadata.is_file()
        || signature_metadata.file_type().is_symlink()
    {
        return Err("Updater artifacts and signatures must be regular non-link files.".to_owned());
    }
    let public_key = parse_public_key(encoded_public_key)?;
    let encoded_signature = std::fs::read_to_string(signature_path)
        .map_err(|error| format!("Could not read updater signature: {error}"))?;
    let signature_text = decode_utf8(encoded_signature.trim(), "updater signature")?;
    let signature = Signature::decode(&signature_text)
        .map_err(|error| format!("Updater signature is invalid: {error}"))?;
    let mut artifact = std::fs::File::open(artifact)
        .map_err(|error| format!("Could not open updater artifact: {error}"))?;
    let mut verifier = public_key
        .verify_stream(&signature)
        .map_err(|error| format!("Updater signature verification failed: {error}"))?;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = artifact
            .read(&mut buffer)
            .map_err(|error| format!("Could not read updater artifact: {error}"))?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier
        .finalize()
        .map_err(|error| format!("Updater signature verification failed: {error}"))
}

fn parse_public_key(encoded_public_key: &str) -> Result<PublicKey, String> {
    let public_key_text = decode_utf8(encoded_public_key, "updater public key")?;
    PublicKey::decode(&public_key_text)
        .map_err(|error| format!("Updater public key is invalid: {error}"))
}

fn decode_utf8(value: &str, description: &str) -> Result<String, String> {
    BASE64_STANDARD
        .decode(value)
        .map_err(|error| format!("The {description} is not valid Base64: {error}"))
        .and_then(|bytes| {
            String::from_utf8(bytes)
                .map_err(|error| format!("The decoded {description} is not UTF-8: {error}"))
        })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
    use tempfile::NamedTempFile;

    use super::{run, verify_updater_signature};

    const PUBLIC_KEY: &str = "untrusted comment: minisign public key E7620F1842B4E81F\n\
                              RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const SIGNATURE: &str = "untrusted comment: signature from minisign secret key\n\
                            RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\n\
                            trusted comment: timestamp:1633700835\tfile:test\tprehashed\n\
                            wLMDjy9FLAuxZ3q4NlEvkgtyhrr0gtTu6KC4KBJdITbbOeAi1zBIYo0v4iTgt8jJpIidRJnp94ABQkJAgAooBQ==";

    #[test]
    fn verifies_a_known_prehashed_updater_signature_and_rejects_changes() {
        let mut artifact = NamedTempFile::new().unwrap();
        artifact.write_all(b"test").unwrap();
        let mut signature = NamedTempFile::new().unwrap();
        signature
            .write_all(BASE64_STANDARD.encode(SIGNATURE).as_bytes())
            .unwrap();
        let public_key = BASE64_STANDARD.encode(PUBLIC_KEY);
        verify_updater_signature(artifact.path(), signature.path(), &public_key).unwrap();

        artifact.as_file_mut().set_len(0).unwrap();
        artifact.write_all(b"changed").unwrap();
        assert!(verify_updater_signature(artifact.path(), signature.path(), &public_key).is_err());
    }

    #[test]
    fn rejects_unknown_commands_and_malformed_keys() {
        assert!(run(&["unknown".to_owned()]).is_err());
        assert!(
            run(&[
                "validate-updater-key".to_owned(),
                BASE64_STANDARD.encode(PUBLIC_KEY),
            ])
            .is_ok()
        );
        let artifact = NamedTempFile::new().unwrap();
        let signature = NamedTempFile::new().unwrap();
        assert!(verify_updater_signature(artifact.path(), signature.path(), "not-base64").is_err());
    }
}

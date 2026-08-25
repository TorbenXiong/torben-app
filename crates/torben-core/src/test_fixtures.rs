use std::io::Write;

use sha2::{Digest, Sha256};
use torben_contracts::{TorbenError, TorbenResult};

use crate::{NodeDistribution, node::ArchiveKind};

pub fn node_plugin_target() -> String {
    crate::node_plugin::current_target()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn build_node_archive(
    distribution: &NodeDistribution,
    node_executable: &[u8],
) -> TorbenResult<Vec<u8>> {
    let root = match distribution.archive_kind {
        ArchiveKind::Zip => distribution.archive_name.strip_suffix(".zip"),
        ArchiveKind::TarGz => distribution.archive_name.strip_suffix(".tar.gz"),
        ArchiveKind::TarXz => distribution.archive_name.strip_suffix(".tar.xz"),
    }
    .ok_or_else(|| fixture_error("The Node.js fixture archive name has an invalid suffix."))?;

    let files = if cfg!(windows) {
        vec![
            (format!("{root}/node.exe"), node_executable.to_vec(), 0o755),
            (
                format!("{root}/npm.cmd"),
                b"@echo off\r\necho 11.0.0\r\n".to_vec(),
                0o755,
            ),
            (
                format!("{root}/npx.cmd"),
                b"@echo off\r\necho 11.0.0\r\n".to_vec(),
                0o755,
            ),
        ]
    } else {
        let package_manager = b"#!/bin/sh\nprintf '11.0.0\\n'\n".to_vec();
        vec![
            (format!("{root}/bin/node"), node_executable.to_vec(), 0o755),
            (format!("{root}/bin/npm"), package_manager.clone(), 0o755),
            (format!("{root}/bin/npx"), package_manager, 0o755),
        ]
    };

    match distribution.archive_kind {
        ArchiveKind::Zip => build_zip(files),
        ArchiveKind::TarGz | ArchiveKind::TarXz => build_tar(files, distribution.archive_kind),
    }
}

fn build_zip(files: Vec<(String, Vec<u8>, u32)>) -> TorbenResult<Vec<u8>> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, content, mode) in files {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(mode);
        writer
            .start_file(path, options)
            .map_err(|error| fixture_error(error.to_string()))?;
        writer
            .write_all(&content)
            .map_err(|error| fixture_error(error.to_string()))?;
    }
    writer
        .finish()
        .map(std::io::Cursor::into_inner)
        .map_err(|error| fixture_error(error.to_string()))
}

fn build_tar(
    files: Vec<(String, Vec<u8>, u32)>,
    archive_kind: ArchiveKind,
) -> TorbenResult<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, content, mode) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len().try_into().map_err(|error| {
            fixture_error(format!("The fixture file size is invalid: {error}"))
        })?);
        header.set_mode(mode);
        header.set_cksum();
        builder
            .append_data(&mut header, path, content.as_slice())
            .map_err(|error| fixture_error(error.to_string()))?;
    }
    builder
        .finish()
        .map_err(|error| fixture_error(error.to_string()))?;
    let tar = builder
        .into_inner()
        .map_err(|error| fixture_error(error.to_string()))?;
    if archive_kind == ArchiveKind::TarGz {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(&tar)
            .map_err(|error| fixture_error(error.to_string()))?;
        encoder
            .finish()
            .map_err(|error| fixture_error(error.to_string()))
    } else {
        let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 6);
        encoder
            .write_all(&tar)
            .map_err(|error| fixture_error(error.to_string()))?;
        encoder
            .finish()
            .map_err(|error| fixture_error(error.to_string()))
    }
}

fn fixture_error(reason: impl Into<String>) -> TorbenError {
    TorbenError::new(
        "test_fixture_build_failed",
        "Could not build the Node.js CLI test fixture.",
    )
    .with_detail("reason", reason.into())
}

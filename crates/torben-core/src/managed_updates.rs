use std::{cmp::Ordering, collections::BTreeMap};

use torben_contracts::{
    AppId, ExactVersion, InstallRecord, InstallScope, ManagedUpdateCandidate, SelectionRecord,
    VersionDescriptor,
};

pub(crate) fn candidates_for_app(
    app_id: &AppId,
    installations: &[InstallRecord],
    selections: &[SelectionRecord],
    versions: &[VersionDescriptor],
    automatic: bool,
) -> Vec<ManagedUpdateCandidate> {
    let mut installed_by_channel: BTreeMap<String, &InstallRecord> = BTreeMap::new();
    for record in installations
        .iter()
        .filter(|record| record.app_id == *app_id && record.scope == InstallScope::Managed)
    {
        let channel = update_channel(app_id, &record.version);
        let replace = installed_by_channel.get(&channel).is_none_or(|current| {
            compare_exact_versions(&record.version, &current.version).is_gt()
        });
        if replace {
            installed_by_channel.insert(channel, record);
        }
    }
    let selected = selections
        .iter()
        .find(|selection| selection.app_id == *app_id)
        .map(|selection| &selection.version)
        .filter(|version| {
            installations.iter().any(|record| {
                record.app_id == *app_id
                    && &record.version == *version
                    && record.scope == InstallScope::Managed
            })
        });
    let mut candidates = Vec::new();
    for (channel, installed) in installed_by_channel {
        let available = versions
            .iter()
            .filter(|version| update_channel(app_id, &version.version) == channel)
            .max_by(|left, right| compare_exact_versions(&left.version, &right.version));
        let Some(available) = available else {
            continue;
        };
        if !compare_exact_versions(&available.version, &installed.version).is_gt() {
            continue;
        }
        candidates.push(ManagedUpdateCandidate {
            app_id: app_id.clone(),
            channel,
            installed_version: installed.version.clone(),
            available_version: available.version.clone(),
            selected_version: selected
                .filter(|version| {
                    update_channel(app_id, version) == update_channel(app_id, &installed.version)
                })
                .cloned(),
            released_at: available.released_at.clone(),
            recommended: available.recommended,
            automatic,
        });
    }
    candidates.sort_by(|left, right| {
        left.app_id
            .cmp(&right.app_id)
            .then_with(|| left.channel.cmp(&right.channel))
    });
    candidates
}

pub(crate) fn compare_exact_versions(left: &ExactVersion, right: &ExactVersion) -> Ordering {
    let left = left.as_semver();
    let right = right.as_semver();
    left.major
        .cmp(&right.major)
        .then_with(|| left.minor.cmp(&right.minor))
        .then_with(|| left.patch.cmp(&right.patch))
        .then_with(|| left.pre.cmp(&right.pre))
        .then_with(|| compare_build_metadata(left.build.as_str(), right.build.as_str()))
}

fn compare_build_metadata(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        match (left.next(), right.next()) {
            (Some(left), Some(right)) => {
                let ordering = match (left.parse::<u64>(), right.parse::<u64>()) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    _ => left.cmp(right),
                };
                if !ordering.is_eq() {
                    return ordering;
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

pub(crate) fn update_channel(app_id: &AppId, version: &ExactVersion) -> String {
    let version = version.as_semver();
    if app_id.as_str() == "python" {
        format!("{}.{}", version.major, version.minor)
    } else {
        version.major.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use torben_contracts::{
        AppId, ExactVersion, InstallRecord, InstallScope, SelectionRecord, SourceId,
        VersionDescriptor,
    };

    use super::{candidates_for_app, compare_exact_versions};

    fn installation(app: &AppId, version: &str) -> InstallRecord {
        InstallRecord {
            app_id: app.clone(),
            version: ExactVersion::from_str(version).unwrap(),
            source_id: SourceId::new(format!("{}.official", app.as_str())).unwrap(),
            scope: InstallScope::Managed,
            install_path: format!("fixture/{version}"),
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        }
    }

    fn available(version: &str) -> VersionDescriptor {
        VersionDescriptor {
            version: ExactVersion::from_str(version).unwrap(),
            lts_name: None,
            released_at: "2026-08-24T00:00:00Z".to_owned(),
            recommended: true,
        }
    }

    #[test]
    fn preserves_runtime_release_lines_and_selected_channel() {
        let node = AppId::new("node").unwrap();
        let installations = [
            installation(&node, "22.20.0"),
            installation(&node, "24.19.0"),
        ];
        let selections = [SelectionRecord {
            app_id: node.clone(),
            version: ExactVersion::from_str("22.20.0").unwrap(),
        }];
        let versions = [
            available("22.22.3"),
            available("24.20.1"),
            available("26.7.0"),
        ];
        let candidates = candidates_for_app(&node, &installations, &selections, &versions, true);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].channel, "22");
        assert_eq!(candidates[0].available_version.to_string(), "22.22.3");
        assert_eq!(
            candidates[0].selected_version.as_ref().unwrap().to_string(),
            "22.20.0"
        );
        assert_eq!(candidates[1].channel, "24");
        assert!(candidates[1].selected_version.is_none());
        assert!(candidates.iter().all(|candidate| candidate.automatic));
    }

    #[test]
    fn python_preserves_major_minor_and_ignores_external_installations() {
        let python = AppId::new("python").unwrap();
        let mut external = installation(&python, "3.14.6");
        external.scope = InstallScope::External;
        let installations = [installation(&python, "3.13.15"), external];
        let versions = [available("3.13.16"), available("3.14.7")];
        let selections = [SelectionRecord {
            app_id: python.clone(),
            version: ExactVersion::from_str("3.14.6").unwrap(),
        }];
        let candidates = candidates_for_app(&python, &installations, &selections, &versions, false);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].channel, "3.13");
        assert_eq!(candidates[0].available_version.to_string(), "3.13.16");
        assert!(candidates[0].selected_version.is_none());
    }

    #[test]
    fn packaging_build_metadata_advances_deterministically() {
        let older = ExactVersion::from_str("2.55.0+windows.4").unwrap();
        let newer = ExactVersion::from_str("2.55.0+windows.5").unwrap();
        assert!(compare_exact_versions(&newer, &older).is_gt());
        let nine = ExactVersion::from_str("2.55.0+windows.9").unwrap();
        let ten = ExactVersion::from_str("2.55.0+windows.10").unwrap();
        assert!(compare_exact_versions(&ten, &nine).is_gt());
    }
}

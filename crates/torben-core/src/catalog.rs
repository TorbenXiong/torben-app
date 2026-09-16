use torben_contracts::{AppId, ApplicationDescriptor, InstallSource, SourceId, TorbenResult};

#[allow(clippy::too_many_lines)]
pub fn applications() -> TorbenResult<Vec<ApplicationDescriptor>> {
    Ok(vec![
        app(
            "node",
            "Node.js",
            "JavaScript runtime with managed LTS and Current releases.",
            &["runtime", "development"],
            &[
                "versions",
                "install",
                "select",
                "uninstall",
                "external-detection",
            ],
            true,
        )?,
        app(
            "temurin",
            "Java",
            "OpenJDK builds from Adoptium.",
            &["runtime", "development"],
            &[
                "versions",
                "install",
                "select",
                "uninstall",
                "external-detection",
            ],
            true,
        )?,
        app(
            "python",
            "Python",
            "The Python programming language.",
            &["runtime", "development"],
            &[
                "versions",
                "install",
                "select",
                "uninstall",
                "external-detection",
            ],
            true,
        )?,
        app(
            "rust",
            "Rust",
            "The Rust programming language with rustc and Cargo.",
            &["runtime", "development"],
            &[
                "versions",
                "install",
                "select",
                "uninstall",
                "external-detection",
            ],
            true,
        )?,
        app(
            "mysql",
            "MySQL",
            "MySQL Community Server with managed client and server binaries.",
            &["database", "development"],
            &[
                "versions",
                "install",
                "select",
                "uninstall",
                "external-detection",
            ],
            true,
        )?,
        app(
            "redis",
            "Redis",
            "Redis-compatible in-memory datastore for Windows development.",
            &["database", "development"],
            &[
                "versions",
                "install",
                "select",
                "uninstall",
                "external-detection",
            ],
            true,
        )?,
        app(
            "postgresql",
            "PostgreSQL",
            "PostgreSQL server and command-line tools for Windows development.",
            &["database", "development"],
            &[
                "versions",
                "install",
                "select",
                "uninstall",
                "external-detection",
            ],
            true,
        )?,
        app(
            "git",
            "Git",
            "Official Git command-line releases with managed terminal selection.",
            &["tool", "development"],
            &[
                "versions",
                "install",
                "select",
                "uninstall",
                "external-detection",
            ],
            false,
        )?,
        app(
            "vscode",
            "Visual Studio Code",
            "Microsoft's official cross-platform code editor distribution.",
            &["editor", "development"],
            &[
                "versions",
                "install",
                "select",
                "uninstall",
                "external-detection",
            ],
            false,
        )?,
        app(
            "codex",
            "Codex CLI",
            "OpenAI's official coding agent command-line client.",
            &["ai", "development"],
            &[
                "versions",
                "install",
                "select",
                "uninstall",
                "external-detection",
            ],
            false,
        )?,
    ])
}

pub fn sources(applications: &[ApplicationDescriptor]) -> TorbenResult<Vec<InstallSource>> {
    let mut sources = applications
        .iter()
        .flat_map(|application| application.sources.iter().cloned())
        .collect::<Vec<_>>();
    for (id, display_name) in [
        ("source.winget", "winget"),
        ("source.homebrew", "Homebrew"),
        ("source.apt", "apt"),
        ("source.dnf", "DNF"),
    ] {
        sources.push(InstallSource {
            id: SourceId::new(id)?,
            display_name: display_name.to_owned(),
            managed: false,
        });
    }
    Ok(sources)
}

fn app(
    id: &str,
    name: &str,
    summary: &str,
    categories: &[&str],
    capabilities: &[&str],
    available: bool,
) -> TorbenResult<ApplicationDescriptor> {
    Ok(ApplicationDescriptor {
        id: AppId::new(id)?,
        display_name: name.to_owned(),
        summary: summary.to_owned(),
        categories: categories.iter().map(ToString::to_string).collect(),
        capabilities: if available {
            capabilities.iter().map(ToString::to_string).collect()
        } else {
            Vec::new()
        },
        sources: if available {
            vec![InstallSource {
                id: SourceId::new(match id {
                    "redis" => "redis.windows".to_owned(),
                    "postgresql" => "postgresql.edb".to_owned(),
                    _ => format!("{id}.official"),
                })?,
                display_name: match id {
                    "redis" => "Redis for Windows community build".to_owned(),
                    "postgresql" => "EDB PostgreSQL Windows distribution".to_owned(),
                    _ => "Official archive".to_owned(),
                },
                managed: true,
            }]
        } else {
            Vec::new()
        },
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn node_temurin_and_python_are_available() {
        let applications = super::applications().unwrap();

        assert_eq!(applications.len(), 10);
        for app_id in [
            "node",
            "temurin",
            "python",
            "rust",
            "mysql",
            "redis",
            "postgresql",
        ] {
            let application = applications
                .iter()
                .find(|application| application.id.as_str() == app_id)
                .unwrap();
            assert!(!application.capabilities.is_empty());
            assert!(!application.sources.is_empty());
        }
        assert!(
            applications
                .iter()
                .filter(|application| !matches!(
                    application.id.as_str(),
                    "node" | "temurin" | "python" | "rust" | "mysql" | "redis" | "postgresql"
                ))
                .all(|application| {
                    application.capabilities.is_empty() && application.sources.is_empty()
                })
        );
    }

    #[test]
    fn unavailable_applications_do_not_publish_managed_sources() {
        let applications = super::applications().unwrap();
        let sources = super::sources(&applications).unwrap();

        assert_eq!(sources.len(), 11);
        assert_eq!(sources.iter().filter(|source| source.managed).count(), 7);
        assert!(
            sources
                .iter()
                .any(|source| source.id.as_str() == "temurin.official")
        );
        assert!(
            sources
                .iter()
                .any(|source| source.id.as_str() == "python.official")
        );
    }
}

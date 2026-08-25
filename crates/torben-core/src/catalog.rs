use torben_contracts::{AppId, ApplicationDescriptor, InstallSource, SourceId, TorbenResult};

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
            "Eclipse Temurin",
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
            true,
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
            true,
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
            true,
        )?,
    ])
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
        capabilities: capabilities.iter().map(ToString::to_string).collect(),
        sources: if available {
            vec![InstallSource {
                id: SourceId::new(format!("{id}.official"))?,
                display_name: "Official archive".to_owned(),
                managed: true,
            }]
        } else {
            Vec::new()
        },
    })
}

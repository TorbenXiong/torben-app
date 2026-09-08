fn main() {
    #[cfg(windows)]
    {
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            let windows = tauri_build::WindowsAttributes::new()
                .app_manifest(include_str!("windows-app-manifest.xml"));
            tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
                .expect("failed to build Torben App with the Windows elevation manifest");
        } else {
            tauri_build::build();
        }
    }

    #[cfg(not(windows))]
    tauri_build::build();
}

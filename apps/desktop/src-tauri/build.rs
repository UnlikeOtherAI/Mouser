use std::path::PathBuf;

fn main() {
    stage_mouserd_sidecar();
    tauri_build::build();
}

fn stage_mouserd_sidecar() {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_else(|| ".".into()));
    let workspace = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.clone());
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "release".to_string());
    let exe_name = if cfg!(windows) {
        "mouserd.exe"
    } else {
        "mouserd"
    };
    let source = workspace.join("target").join(&profile).join(exe_name);
    let binaries = manifest_dir.join("binaries");
    let destination = binaries.join(exe_name);

    if let Err(e) = std::fs::create_dir_all(&binaries) {
        println!(
            "cargo:warning=failed to create sidecar directory {}: {e}",
            binaries.display()
        );
        return;
    }

    if source.exists() {
        if let Err(e) = std::fs::copy(&source, &destination) {
            println!(
                "cargo:warning=failed to stage Mouser daemon sidecar {} -> {}: {e}",
                source.display(),
                destination.display()
            );
        }
    } else {
        let profile_flag = if profile == "release" {
            " --release"
        } else {
            ""
        };
        println!(
            "cargo:warning=Mouser daemon sidecar not found at {}; build it first with `cargo build -p mouser-engine{} --bin mouserd`",
            source.display(),
            profile_flag
        );
    }
}

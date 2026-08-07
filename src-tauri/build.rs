use std::{env, fs, path::PathBuf};

fn main() {
    let package_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("..")
        .join("package.json");
    println!("cargo:rerun-if-changed={}", package_path.display());

    let package: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&package_path).expect("could not read package.json"),
    )
    .expect("package.json is not valid JSON");
    let version = package
        .get("version")
        .and_then(serde_json::Value::as_str)
        .expect("package.json does not contain a string version");
    println!("cargo:rustc-env=OPENSHORES_LAUNCHER_VERSION={version}");

    tauri_build::build()
}

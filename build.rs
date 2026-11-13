fn main() {
    // The exe's version resource is written by hand in app.rc: keep it in step with Cargo.toml
    // (the updater compares Cargo's version, Explorer and the installer show the resource's).
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let rc = std::fs::read_to_string("app.rc").unwrap();
    let numbers = version.replace('.', ",") + ",0";
    assert!(
        rc.contains(&format!("\"FileVersion\", \"{version}\"")) && rc.contains(&format!("FILEVERSION {numbers}")),
        "app.rc's version is not {version}: update FILEVERSION, PRODUCTVERSION and their strings"
    );
    println!("cargo:rerun-if-changed=app.rc");
    // Embeds the icon (resource id 1) and the manifest (common controls v6 for the edit cue banner).
    embed_resource::compile("app.rc", embed_resource::NONE).manifest_optional().unwrap();
}

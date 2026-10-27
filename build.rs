fn main() {
    // Embeds the icon (resource id 1) and the manifest (common controls v6 for the edit cue banner).
    embed_resource::compile("app.rc", embed_resource::NONE).manifest_optional().unwrap();
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winres::WindowsResource::new();
        // Use an absolute path or a path relative to the manifest directory
        let ico = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets")
            .join("rake_logo.ico");

        if ico.exists() {
            res.set_icon(ico.to_str().unwrap());
            res.compile().unwrap();
        } else {
            eprintln!("Icon file not found: {:?}", ico);
        }
    }
}

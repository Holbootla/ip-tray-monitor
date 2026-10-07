fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.manifest");
    println!("cargo:rerun-if-changed=assets");
    // Resources (icons, manifest, version info) only matter for Windows builds.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("res/app.rc", embed_resource::NONE);
    }
}

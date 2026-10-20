fn main() {
    println!("cargo:rerun-if-env-changed=ARCUS_CLI_TAG");
    let tag = std::env::var("ARCUS_CLI_TAG").unwrap_or_else(|_| "dev".into());
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    println!("cargo:rustc-env=ARCUS_CLI_VERSION={ver} ({tag})");
}

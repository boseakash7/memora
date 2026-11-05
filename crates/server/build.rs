// The TV page is embedded from `ui/dist-tv` (built by `npm run build` in `ui/`). Make sure the
// folder exists so the server compiles even before the page has been built; in debug builds
// the files are then read from disk at run time.
fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist-tv");
    let _ = std::fs::create_dir_all(&dir);
    println!("cargo:rerun-if-changed=../../ui/dist-tv");
}

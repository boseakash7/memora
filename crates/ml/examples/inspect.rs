//! Print the inputs and outputs of ONNX models: `cargo run -p photos-ml --example inspect -- <model.onnx>...`
fn main() -> ort::Result<()> {
    let env = ort::init().with_telemetry(false).build()?;
    for path in std::env::args().skip(1) {
        let s = ort::session::Session::builder(&env)?.commit_from_file(&path)?;
        println!("{path}");
        for i in s.inputs() {
            println!("  in  {} {}", i.name(), i.dtype());
        }
        for o in s.outputs() {
            println!("  out {} {}", o.name(), o.dtype());
        }
    }
    Ok(())
}

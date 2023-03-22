use std::path::Path;

fn main() -> Result<(), anyhow::Error> {
    println!("cargo:rerun-if-changed=import.h");

    let bindings = bindgen::builder()
        .header(concat!(env!("CARGO_MANIFEST_DIR"), "/import.h"))
        .rustified_enum(".*")
        .generate()?;
    bindings.write_to_file(Path::new(&std::env::var("OUT_DIR").unwrap()).join("bindings.rs"))?;
    Ok(())
}

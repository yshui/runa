use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

use runa_wayland_scanner::generate_protocol;
use runa_wayland_spec_parser as spec;

fn main() {
    use std::io::Write;

    let protocols = std::path::Path::new("spec");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    println!("cargo:rerun-if-changed={}", protocols.display());
    let generate_from_dir = |name: &str| {
        let dest_path = std::path::Path::new(&out_dir).join(&format!("{name}_generated.rs"));
        let mut outfile = std::fs::File::create(dest_path).unwrap();
        let dir_path = protocols.join("wayland-protocols").join(name);
        for dir in std::fs::read_dir(dir_path).unwrap() {
            let dir = dir.unwrap();
            let metadata = dir.metadata().unwrap();
            if !metadata.is_dir() {
                continue
            }
            for file in std::fs::read_dir(dir.path()).unwrap() {
                let file = file.unwrap();
                let metadata = file.metadata().unwrap();
                if !metadata.is_file() {
                    continue
                }
                if file.path().extension() != Some(OsStr::from_bytes(b"xml")) {
                    continue
                }
                let protocol =
                    spec::parse::parse(std::fs::read_to_string(file.path()).unwrap().as_bytes())
                        .unwrap();
                //let source = fmt
                //    .format_tokens(generate_protocol(&protocol).unwrap())
                //    .unwrap();
                let source = generate_protocol(&protocol).unwrap().to_string();
                write!(outfile, "{source}").unwrap();
            }
        }
    };

    let contents = std::fs::read_to_string(protocols.join("wayland.xml")).unwrap();
    let protocol = spec::parse::parse(contents.as_bytes()).unwrap();
    let source = generate_protocol(&protocol).unwrap().to_string();
    //let source = fmt
    //    .format_tokens(generate_protocol(&protocol).unwrap())
    //    .unwrap();

    let dest_path = std::path::Path::new(&out_dir).join("wayland_generated.rs");
    std::fs::write(dest_path, source).unwrap();

    generate_from_dir("stable");
    generate_from_dir("staging");
    generate_from_dir("unstable");
}

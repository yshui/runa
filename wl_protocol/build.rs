use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use wl_scanner::generate_protocol;

fn main() {
    use rust_format::Formatter;
    use std::io::Write;

    let fmt = rust_format::RustFmt::new();
    let protocols =
        std::path::Path::new(&std::env::var("CARGO_WORKSPACE_DIR").unwrap()).join("protocols");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let generate_from_dir = |name: &str| {
        let dest_path =
            std::path::Path::new(&out_dir).join(&format!("{}_generated.rs", name));
        let mut outfile = std::fs::File::create(dest_path).unwrap();
        for dir in std::fs::read_dir(protocols.join("wayland-protocols").join(name)).unwrap() {
            let dir = dir.unwrap();
            let metadata = dir.metadata().unwrap();
            if !metadata.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(dir.path()).unwrap() {
                let file = file.unwrap();
                let metadata = file.metadata().unwrap();
                if !metadata.is_file() {
                    continue;
                }
                if file.path().extension() != Some(OsStr::from_bytes(b"xml")) {
                    continue;
                }
                let protocol =
                    wl_spec::parse::parse(std::fs::read_to_string(file.path()).unwrap().as_bytes())
                        .unwrap();
                //let source = fmt
                //    .format_tokens(generate_protocol(&protocol).unwrap())
                //    .unwrap();
                let source = generate_protocol(&protocol).unwrap().to_string();
                write!(outfile, "{}", source).unwrap();
            }
        }
    };
    let contents = std::fs::read_to_string(&protocols.join("wayland.xml")).unwrap();
    let protocol = wl_spec::parse::parse(contents.as_bytes()).unwrap();
    let source = generate_protocol(&protocol).unwrap().to_string();// fmt
//        .format_tokens(generate_protocol(&protocol).unwrap())
//        .unwrap();

    let dest_path = std::path::Path::new(&out_dir).join("wayland_generated.rs");
    std::fs::write(&dest_path, source).unwrap();

    generate_from_dir("stable");
    generate_from_dir("staging");
    generate_from_dir("unstable");
}

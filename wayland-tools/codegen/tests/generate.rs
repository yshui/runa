use runa_wayland_scanner_codegen::generate_protocol;
#[test]
fn generate() {
    let f = std::fs::File::open(
        std::path::Path::new(&std::env::var("CARGO_WORKSPACE_DIR").unwrap())
            .join("runa-wayland-protocols")
            .join("spec")
            .join("wayland.xml"),
    )
    .unwrap();
    let proto = spec_parser::parse::parse(f).unwrap();

    generate_protocol(&proto).unwrap();
}

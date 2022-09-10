use wl_scanner::generate_protocol;
#[test]
fn generate() {
    let f = std::fs::File::open(
        std::path::Path::new(&std::env::var("CARGO_WORKSPACE_DIR").unwrap())
            .join("protocols")
            .join("wayland.xml"),
    )
    .unwrap();
    let proto = wl_spec::parse::parse(f).unwrap();

    eprintln!("{}", generate_protocol(&proto).unwrap());
}

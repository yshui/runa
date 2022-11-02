#[test]
fn parse() {
    let file = std::fs::File::open(concat!(
        env!("CARGO_WORKSPACE_DIR"),
        "/protocols/wayland.xml"
    ))
    .unwrap();
    eprintln!("{:?}", wl_spec::parse::parse(file).unwrap());
}

#[test]
fn parse() {
    let file = std::fs::File::open(concat!(
        env!("CARGO_WORKSPACE_DIR"),
        "/runa-wayland-protocols/spec/wayland.xml"
    ))
    .unwrap();
    eprintln!(
        "{:?}",
        runa_wayland_spec_parser::parse::parse(file).unwrap()
    );
}

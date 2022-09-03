#[test]
fn parse() {
    let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/protocol/wayland.xml")).unwrap();
    eprintln!("{:?}", wl_spec::parse::parse(file).unwrap());
}

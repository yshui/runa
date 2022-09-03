#[test]
fn parse() {
    let file = std::fs::File::open("/usr/share/wayland-protocols/stable/xdg-shell/xdg-shell.xml").unwrap();
    eprintln!("{:?}", wl_spec::parse::parse(file).unwrap());
}

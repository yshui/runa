extern crate proc_macro;
use proc_macro_error::ResultExt;

#[proc_macro]
pub fn generate_protocol(tokens: proc_macro::TokenStream) -> proc_macro::TokenStream {
    use std::io::Read;
    let tokens2 = tokens.clone();
    let path = syn::parse_macro_input!(tokens2 as syn::LitStr);
    let path = std::path::Path::new(&std::env::var("CARGO_WORKSPACE_DIR").unwrap())
        .join(&path.value())
        .to_owned();
    let tokens: proc_macro2::TokenStream = tokens.into();
    let mut file = std::fs::File::open(path)
        .map_err(|e| syn::Error::new_spanned(&tokens, e))
        .unwrap_or_abort();
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|e| syn::Error::new_spanned(&tokens, e))
        .unwrap_or_abort();
    let protocol = wl_spec::parse::parse(contents.as_bytes()).unwrap();
    wl_scanner::generate_protocol(&protocol).unwrap().into()
}

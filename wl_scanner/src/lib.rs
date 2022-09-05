use heck::ToPascalCase;
use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};
use thiserror::Error;
use wl_spec::protocol::{Interface, Message};
#[derive(Error, Debug)]
pub enum Error {
    #[error("Destructor is not a valid argument type")]
    InvalidArgumentType,
}
type Result<T, E = Error> = std::result::Result<T, E>;
pub fn generate_arg_type(ty: &wl_spec::protocol::Type, is_owned: bool) -> Result<TokenStream> {
    use wl_spec::protocol::Type::*;
    Ok(match ty {
        Int => quote!(i32),
        Uint => quote!(u32),
        Fixed => quote!(::wl_scanner_support::wayland_types::Fixed),
        Array => quote!(&'a [u8]),
        Fd => quote!(RawFd),
        String => {
            if is_owned {
                quote!(::std::ffi::OsString)
            } else {
                quote!(&'a ::std::ffi::OsStr)
            }
        }
        Object => quote!(::wl_scanner_support::wayland_types::Object),
        NewId => quote!(::wl_scanner_support::wayland_types::NewId),
        Destructor => return Err(Error::InvalidArgumentType),
    })
}

pub fn type_is_borrowed(ty: &wl_spec::protocol::Type) -> bool {
    use wl_spec::protocol::Type::*;
    match ty {
        Int | Uint | Fixed | Fd | Object | NewId => false,
        String | Array => true,
        Destructor => false,
    }
}
pub fn generate_message_variant(request: &Message, is_owned: bool) -> Result<TokenStream> {
    let args = &request.args;
    let args: Result<TokenStream> = args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            let ty = generate_arg_type(&arg.typ, is_owned)?;
            Ok(quote! {
                #name: #ty,
            })
        })
        .collect();
    let args = args?;
    let name = format_ident!("{}", request.name.as_str().to_pascal_case());
    Ok(quote! {
        #name {
            #args
        },
    })
}
pub fn generate_serialize_for_type(name: &Ident, ty: &wl_spec::protocol::Type) -> TokenStream {
    use wl_spec::protocol::Type::*;
    if let Fd = ty {
        return quote! {
            ::wl_scanner_support::ready!(self.write.poll_write_with_fd(&[], #name))?;
        };
    }
    let get = match ty {
        Int | Uint => quote! {
            let buf = #name.to_ne_bytes();
        },
        Fixed | Object | NewId => quote! {
            let buf = #name.0.to_ne_bytes();
        },
        Fd => unreachable!(),
        String => quote! {
            use std::os::unix::ffi::OsStrExt;
            let buf = #name.as_bytes();
        },
        Array => quote! {
            let buf = #name;
        },
        Destructor => quote! {},
    };
    quote! {
        #get
        loop {
            let offset = self.offset;
            let written = ::wl_scanner_support::ready!(self.writer.as_mut().poll_write(cx, &buf[offset..]))?;
            if written == 0 {
                return ::std::task::Poll::Ready(Err(::std::io::Error::new(::std::io::ErrorKind::WriteZero, "Write zero").into()));
            }
            self.offset += written;
            if self.offset == buf.len() {
                break;
            }
        }
    }
}
pub fn generate_message_variant_serialize(request: &Message, parent: &Ident) -> Result<TokenStream> {
    let arg_list: TokenStream = request
        .args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            quote! {
                #name,
            }
        })
        .collect();
    let serialize: TokenStream = request
        .args
        .iter()
        .enumerate()
        .map(|(i, arg)| {
            let name = format_ident!("{}", arg.name);
            let i = i as u32;
            let serialize = generate_serialize_for_type(&name, &arg.typ);
            quote! {
                #i => {
                    #serialize
                }
            }
        })
        .collect();
    let nargs = request.args.len() as u32;
    let var_name = format_ident!("{}", request.name.to_pascal_case());
    Ok(quote! {
        #parent::#var_name { #arg_list } => {
            loop {
                match self.index {
                    #serialize
                    _ => break,
                }
                self.offset = 0;
                self.index += 1;
            }
        }
    })
}

pub fn generate_all_messages(iface: &Interface) -> Result<TokenStream> {
    let requests = iface
        .requests
        .iter()
        .map(|v| generate_message_variant(v, false))
        .collect::<Result<TokenStream>>()?;
    let events = iface
        .events
        .iter()
        .map(|v| generate_message_variant(v, false))
        .collect::<Result<TokenStream>>()?;
    let name = format_ident!("{}", iface.name);
    let request_serialize = iface
        .requests
        .iter()
        .map(|v| generate_message_variant_serialize(v, &format_ident!("Request")))
        .collect::<Result<TokenStream>>()?;
    let event_serialize = iface
        .events
        .iter()
        .map(|v| generate_message_variant_serialize(v, &format_ident!("Event")))
        .collect::<Result<TokenStream>>()?;

    let request_borrowed = iface
        .requests
        .iter()
        .any(|v| v.args.iter().any(|arg| type_is_borrowed(&arg.typ)));
    let request_lifetime = if request_borrowed {
        quote! {
            <'a>
        }
    } else {
        quote! {}
    };
    let event_borrowed = iface
        .events
        .iter()
        .any(|v| v.args.iter().any(|arg| type_is_borrowed(&arg.typ)));
    let event_lifetime = if event_borrowed {
        quote! {
            <'a>
        }
    } else {
        quote! {}
    };
    let deserialize = quote! {};

    Ok(quote! {
        pub mod #name {
            pub enum Request #request_lifetime {
                #requests
            }
            pub enum Event #event_lifetime {
                #events
            }
            pub struct RequestSerialize<'a, T: 'a> {
                request: &'a Request #request_lifetime,
                writer: ::std::pin::Pin<&'a mut T>,
                index: u32, // current field being serialized
                offset: usize, // offset into the current field
            }
            impl<'a, T: ::wl_scanner_support::io::AsyncWriteWithFds + 'a> ::wl_scanner_support::io::Serialize<'a, T> for Request #request_lifetime {
                type Error = ::wl_scanner_support::Error;
                type Serialize = RequestSerialize<'a, T>;
                fn serialize(&'a self, writer: ::std::pin::Pin<&'a mut T>)
                -> Self::Serialize{
                    RequestSerialize {
                        request: self,
                        writer: writer,
                        index: 0,
                        offset: 0,
                    }
                }
            }
            impl<'a, T: ::wl_scanner_support::io::AsyncWriteWithFds + 'a> ::std::future::Future for RequestSerialize<'a, T> {
                type Output = ::std::result::Result<(), ::wl_scanner_support::Error>;
                fn poll(mut self: ::std::pin::Pin<&mut Self>, cx: &mut ::std::task::Context<'_>) -> ::std::task::Poll<Self::Output> {
                    match &self.request {
                        #request_serialize
                    }
                    ::std::task::Poll::Ready(Ok(()))
                }
            }
            pub struct EventSerialize<'a, T: 'a> {
                request: &'a Event #event_lifetime,
                writer: ::std::pin::Pin<&'a mut T>,
                index: u32,
                offset: usize,
            }
            impl<'a, T: ::wl_scanner_support::io::AsyncWriteWithFds + 'a> ::wl_scanner_support::io::Serialize<'a, T> for Event #event_lifetime {
                type Serialize = EventSerialize<'a, T>;
                type Error = ::wl_scanner_support::Error;
                fn serialize(&'a self, writer: ::std::pin::Pin<&'a mut T>)
                -> Self::Serialize{
                    EventSerialize {
                        request: self,
                        writer: writer,
                        index: 0,
                        offset: 0,
                    }
                }
            }
            impl<'a, T: ::wl_scanner_support::io::AsyncWriteWithFds + 'a> ::std::future::Future for EventSerialize<'a, T> {
                type Output = ::std::result::Result<(), ::wl_scanner_support::Error>;
                fn poll(mut self: ::std::pin::Pin<&mut Self>, cx: &mut ::std::task::Context<'_>) -> ::std::task::Poll<Self::Output> {
                    match &self.request {
                        #event_serialize
                    }
                    ::std::task::Poll::Ready(Ok(()))
                }
            }

            //impl<'de> ::wl_scanner_support::io::Deserialize<'de> for Event<'de> {
            //    type Error = ::wl_scanner_support::Error;
            //    fn deserialize<D>(&self, writer: ::std::pin::Pin<&'de mut D>, cx: &mut ::std::task::Context<'_>)
            //    -> ::std::task::Poll<::std::result::Result<Self, Self::Error>>
            //    where D: ::wl_scanner_support::io::Deserializer {
            //        #deserialize
            //    }
            //}
            //impl<'de> ::wl_scanner_support::io::Deserialize<'de> for Request<'de> {
            //    type Error = ::wl_scanner_support::Error;
            //    fn deserialize<D>(&self, writer: ::std::pin::Pin<&'de mut D>, cx: &mut ::std::task::Context<'_>)
            //    -> ::std::task::Poll<::std::result::Result<Self, Self::Error>>
            //    where D: ::wl_scanner_support::io::Deserializer {
            //        #deserialize
            //    }
            //}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generate_interface() {
        use rust_format::{Formatter, RustFmt};
        let formatter = RustFmt::new();
        let workspace: std::path::PathBuf = std::env::var("CARGO_WORKSPACE_DIR").unwrap().into();
        let proto = wl_spec::parse::parse(
            std::fs::File::open(workspace.join("protocols").join("wayland.xml")).unwrap(),
        )
        .unwrap();
        eprintln!(
            "{}",
            formatter
                .format_tokens(generate_all_messages(&proto.interfaces[0]).unwrap())
                .unwrap()
        );
    }
}

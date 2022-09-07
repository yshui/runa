use heck::ToPascalCase;
use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};
use thiserror::Error;
use wl_spec::protocol::{Interface, Message, Protocol};
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
        Fd => quote!(::std::os::unix::prelude::RawFd),
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

fn fixed_length_write() -> TokenStream {
    quote! {
        loop {
            let offset = self.offset;
            if offset >= 4 {
                break;
            }
            let to_write = &buf[offset..];
            let written = ::wl_scanner_support::ready!(self.writer.as_mut().poll_write(cx, to_write))?;
            if written == 0 {
                return ::std::task::Poll::Ready(
                    Err(::std::io::Error::new(::std::io::ErrorKind::WriteZero, "Write zero").into()));
            }
            self.offset += written;
        }
    }
}
pub fn generate_serialize_for_type(
    name: &TokenStream,
    ty: &wl_spec::protocol::Type,
) -> TokenStream {
    use wl_spec::protocol::Type::*;
    if let Fd = ty {
        return quote! {
            use ::wl_scanner_support::io::AsyncWriteWithFds;
            let fd = #name;
            ::wl_scanner_support::ready!(self.writer.as_mut().poll_write_with_fds(cx, &[], &[fd]))?;
        };
    }
    let null_term_length = if let String = ty { 1usize } else { 0 };
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
    let fixed_length_write = fixed_length_write();
    match ty {
        Int | Uint | Fixed | Object | NewId => quote! {
            #get
            #fixed_length_write
        },
        String | Array => quote! {
            #get
            let aligned_len = ((buf.len() + 3) & !3) + 4;
            let align_buf = [0; 3];
            let len_buf = (buf.len() + #null_term_length).to_ne_bytes();
            loop {
                let offset = self.offset;
                // [0, 4): length
                // [4, buf.len()+4): buf
                // [buf.len()+4, aligned_len): alignment
                let to_write = if offset < 4 {
                    &len_buf[offset..]
                } else if offset - 4 < buf.len() {
                    &buf[offset - 4..]
                } else if offset < aligned_len {
                    &align_buf[..aligned_len-offset]
                } else {
                    break;
                };
                let written = ::wl_scanner_support::ready!(self.writer.as_mut().poll_write(cx, to_write))?;
                if written == 0 {
                    return ::std::task::Poll::Ready(
                        Err(::std::io::Error::new(::std::io::ErrorKind::WriteZero, "Write zero").into()));
                }
                self.offset += written;
            }
        },
        Fd => unreachable!(),
        Destructor => quote! {},
    }
}
pub fn generate_message_variant(
    opcode: u16,
    request: &Message,
    is_owned: bool,
    parent: &Ident,
) -> Result<(TokenStream, TokenStream)> {
    let args = &request.args;
    let args: Result<TokenStream> = args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            let ty = generate_arg_type(&arg.typ, is_owned)?;
            Ok(quote! {
                pub #name: #ty,
            })
        })
        .collect();
    let args = args?;
    let pname = request.name.as_str().to_pascal_case();
    let name = format_ident!("{}", pname);
    let is_borrowed = request.args.iter().any(|arg| type_is_borrowed(&arg.typ));
    let lifetime = if is_borrowed {
        quote! {
            <'a>
        }
    } else {
        quote! {}
    };
    let serialize_name = format_ident!("{}Serialize", pname);
    let deserialize_name = format_ident!("{}Deserialize", pname);
    let public = quote! {
        pub struct #name #lifetime {
            #args
        }
        impl<'a, T: ::wl_scanner_support::io::AsyncWriteWithFds + 'a>
        ::wl_scanner_support::io::Serialize<'a, T> for #name #lifetime {
            type Error = ::wl_scanner_support::Error;
            type Serialize = super::internal::#serialize_name <'a, T>;
            fn serialize(&'a self, writer: ::std::pin::Pin<&'a mut T>)
            -> Self::Serialize {
                Self::Serialize {
                    request: self,
                    writer,
                    index: -1, // Start with -1 to write the message prefix
                    offset: 0,
                }
            }
        }

        impl<'a, D> ::wl_scanner_support::io::Deserialize<'a, D> for #name #lifetime
        where
            D: ::wl_scanner_support::io::Deserializer<'a> + 'a {
            type Error = ::wl_scanner_support::Error;
            type Deserialize = super::internal::#deserialize_name <'a, D>;
            fn deserialize(deserializer: ::std::pin::Pin<&'a mut D>)
            -> Self::Deserialize {
                Self::Deserialize {
                    // Safety: &mut is not used but just turned into a raw pointer directly.
                    de: unsafe { deserializer.get_unchecked_mut().into() },
                    pending: ::wl_scanner_support::io::DeserializerFutureHolder::None,
                    tmp: ::std::mem::MaybeUninit::uninit(),
                    index: 0,
                    _pin: ::std::default::Default::default(),
                }
            }
        }
    };

    let serialize = generate_message_variant_serialize(opcode, request)?;
    let private = quote! {
        pub struct #serialize_name <'a, T> {
            pub(super) request: &'a super::#parent::#name #lifetime,
            pub(super) writer: ::std::pin::Pin<&'a mut T>,
            pub(super) index: i32, // current field being serialized
            pub(super) offset: usize, // offset into the current field
        }
        impl<'a, T> ::std::future::Future for #serialize_name<'a, T>
        where
            T: ::wl_scanner_support::io::AsyncWriteWithFds {
            type Output = ::std::result::Result<(), ::wl_scanner_support::Error>;
            fn poll(mut self: ::std::pin::Pin<&mut Self>, cx: &mut ::std::task::Context<'_>) ->
                ::std::task::Poll<Self::Output> {
                #serialize
                ::std::task::Poll::Ready(Ok(()))
            }
        }

        pub struct #deserialize_name <'a, T: ::wl_scanner_support::io::Deserializer<'a>> {
            pub(super) de: ::std::ptr::NonNull<T>,
            pub(super) pending: ::wl_scanner_support::io::DeserializerFutureHolder<'a, T>,
            pub(super) tmp: ::std::mem::MaybeUninit<super::#parent::#name #lifetime>,
            pub(super) index: u32,
            pub(super) _pin: ::std::marker::PhantomPinned,
        }
        impl<'a, D> ::std::future::Future for #deserialize_name<'a, D>
        where
            D: ::wl_scanner_support::io::Deserializer<'a> {
            type Output = ::std::result::Result<super::#parent::#name #lifetime, ::wl_scanner_support::Error>;
            fn poll(mut self: ::std::pin::Pin<&mut Self>, cx: &mut ::std::task::Context<'_>) ->
                ::std::task::Poll<Self::Output> {
                todo!()
            }
        }
    };
    Ok((public, private))
}
pub fn generate_message_variant_serialize(opcode: u16, request: &Message) -> Result<TokenStream> {
    let serialize: TokenStream = request
        .args
        .iter()
        .enumerate()
        .map(|(i, arg)| {
            let name = format_ident!("{}", arg.name);
            let name = quote! { self.request.#name };
            let i = i as i32;
            let serialize = generate_serialize_for_type(&name, &arg.typ);
            quote! {
                #i => {
                    #serialize
                }
            }
        })
        .collect();
    // Generate experssion for length calculation
    let fixed_len: u32 = request
        .args
        .iter()
        .filter(|arg| match arg.typ {
            wl_spec::protocol::Type::Int
            | wl_spec::protocol::Type::Uint
            | wl_spec::protocol::Type::Fixed
            | wl_spec::protocol::Type::Object
            | wl_spec::protocol::Type::String // string and array has a length prefix, so they
            | wl_spec::protocol::Type::Array  // count, too.
            | wl_spec::protocol::Type::NewId => true,
            wl_spec::protocol::Type::Fd
            | wl_spec::protocol::Type::Destructor=> false,
        })
        .count() as u32
        * 4;
    let variable_len: TokenStream = request
        .args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            match &arg.typ {
                wl_spec::protocol::Type::Int
                | wl_spec::protocol::Type::Uint
                | wl_spec::protocol::Type::Fixed
                | wl_spec::protocol::Type::Object
                | wl_spec::protocol::Type::NewId
                | wl_spec::protocol::Type::Fd
                | wl_spec::protocol::Type::Destructor => quote! {},
                wl_spec::protocol::Type::String => quote! {
                    + ((::std::os::unix::ffi::OsStrExt::as_bytes(self.request.#name).len() + 1 + 3) & !3) as u32
                },
                wl_spec::protocol::Type::Array => quote! {
                    + ((self.request.#name.len() + 3) & !3) as u32
                },
            }
        })
        .collect();
    let var_name = format_ident!("{}", request.name.to_pascal_case());
    let fixed_length_write = fixed_length_write();
    Ok(quote! {
        let msg_len = (#fixed_len #variable_len + 8) as u32; // 8 is the header
        let prefix = (msg_len << 16) + (#opcode as u32);
        loop {
            match self.index {
                -1 => {
                    // Write message length
                    let buf = prefix.to_ne_bytes();
                    #fixed_length_write
                }
                #serialize
                _ => break,
            }
            self.offset = 0;
            self.index += 1;
        }
    })
}

fn generate_deserialize_for_type(name: &Ident, ty: &wl_spec::protocol::Type) -> TokenStream {
    quote! {}
}

pub fn generate_message_variant_deserialize(request: &Message) -> Result<TokenStream> {
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
    let deserialize: TokenStream = request
        .args
        .iter()
        .enumerate()
        .map(|(i, arg)| {
            let name = format_ident!("{}", arg.name);
            let i = i as i32;
            let deserialize = generate_deserialize_for_type(&name, &arg.typ);
            quote! {
                #i => {
                    #deserialize
                }
            }
        })
        .collect();
    let var_name = format_ident!("{}", request.name.to_pascal_case());
    Ok(quote! {
        #var_name { #arg_list }
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventOrRequest {
    Event,
    Request,
}

pub fn generate_event_or_request(
    name: &str,
    messages: &[Message],
    event_or_request: EventOrRequest,
) -> Result<(TokenStream, TokenStream)> {
    if messages.is_empty() {
        Ok((quote! {}, quote! {}))
    } else {
        let mod_name = match event_or_request {
            EventOrRequest::Event => format_ident!("events"),
            EventOrRequest::Request => format_ident!("requests"),
        };
        let type_name = match event_or_request {
            EventOrRequest::Event => format_ident!("Event"),
            EventOrRequest::Request => format_ident!("Request"),
        };
        let (public, mut private): (TokenStream, TokenStream) = messages
            .iter()
            .enumerate()
            .map(|(opcode, v)| generate_message_variant(opcode as u16, v, false, &mod_name))
            .collect::<Result<Vec<(TokenStream, TokenStream)>>>()?
            .into_iter()
            .unzip();

        let enum_is_borrowed = messages
            .iter()
            .any(|v| v.args.iter().any(|arg| type_is_borrowed(&arg.typ)));
        let enum_lifetime = if enum_is_borrowed {
            quote! { <'a> }
        } else {
            quote! {}
        };
        let enum_members = messages
            .iter()
            .map(|v| {
                let name = format_ident!("{}", v.name.to_pascal_case());
                let is_borrowed = v.args.iter().any(|arg| type_is_borrowed(&arg.typ));
                let lifetime = if is_borrowed {
                    quote! { <'a> }
                } else {
                    quote! {}
                };
                quote! {
                    #name(#mod_name::#name #lifetime),
                }
            })
            .collect::<TokenStream>();
        let enum_serialize_members = messages
            .iter()
            .map(|v| {
                let name = format_ident!("{}", v.name.to_pascal_case());
                let serialize_name = format_ident!("{}Serialize", name);
                quote! {
                    #name(#serialize_name<'a, T>),
                }
            })
            .collect::<TokenStream>();
        let enum_serialize_name = format_ident!("_{}Serialize", type_name); // Avoid potential name clash
        let enum_serialize_cases = messages
            .iter()
            .map(|v| {
                let name = format_ident!("{}", v.name.to_pascal_case());
                quote! {
                    Self::#name(v) => internal::#enum_serialize_name::#name(v.serialize(writer)),
                }
            })
            .collect::<TokenStream>();
        let enum_serialize_poll_cases = messages
            .iter()
            .map(|v| {
                let name = format_ident!("{}", v.name.to_pascal_case());
                quote! {
                    Self::#name(v) => ::wl_scanner_support::ready!(::std::pin::Pin::new(v).poll(cx)),
                }
            })
            .collect::<TokenStream>();
        let public = quote! {
            pub mod #mod_name { #public }
            pub enum #type_name #enum_lifetime {
                #enum_members
            }
            impl<'a, T: ::wl_scanner_support::io::AsyncWriteWithFds + 'a>
            ::wl_scanner_support::io::Serialize<'a, T> for #type_name #enum_lifetime {
                type Error = ::wl_scanner_support::Error;
                type Serialize = internal::#enum_serialize_name <'a, T>;
                fn serialize(&'a self, writer: ::std::pin::Pin<&'a mut T>)
                -> Self::Serialize {
                    match self {
                        #enum_serialize_cases
                    }
                }
            }
        };
        private.extend(quote!{
            pub enum #enum_serialize_name <'a, T> {
                #enum_serialize_members
            }
            impl<'a, T> ::std::future::Future for #enum_serialize_name<'a, T>
            where
                T: ::wl_scanner_support::io::AsyncWriteWithFds {
                type Output = ::std::result::Result<(), ::wl_scanner_support::Error>;
                fn poll(mut self: ::std::pin::Pin<&mut Self>, cx: &mut ::std::task::Context<'_>) ->
                    ::std::task::Poll<Self::Output> {
                    ::std::task::Poll::Ready(match &mut *self {
                        #enum_serialize_poll_cases
                    })
                }
            }
        });
        Ok((public, private))
    }
}

pub fn generate_interface(iface: &Interface) -> Result<TokenStream> {
    let name = format_ident!("{}", iface.name);
    let version = format_ident!("v{}", iface.version);

    let (requests, requests_private) =
        generate_event_or_request(&iface.name, &iface.requests, EventOrRequest::Request)?;
    let (events, events_private) =
        generate_event_or_request(&iface.name, &iface.events, EventOrRequest::Event)?;

    Ok(quote! {
        pub mod #name {
            pub mod #version {
                #requests
                #events
                #[doc(hidden)]
                mod internal {
                    #requests_private
                    #events_private
                }
            }
        }
    })
}

pub fn generate_protocol(proto: &Protocol) -> Result<TokenStream> {
    let interfaces = proto
        .interfaces
        .iter()
        .map(|v| generate_interface(v))
        .collect::<Result<TokenStream>>()?;
    let name = format_ident!("{}", proto.name);
    Ok(quote! {
        pub mod #name {
            #interfaces
        }
    })
}

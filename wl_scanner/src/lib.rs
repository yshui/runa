use std::collections::HashMap;

use heck::{ToPascalCase, ToShoutySnekCase};
use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};
use syn::Lifetime;
use thiserror::Error;
use wl_spec::protocol::{Enum, Interface, Message, Protocol};
#[derive(Error, Debug)]
pub enum Error {
    #[error("Destructor is not a valid argument type")]
    InvalidArgumentType,

    #[error("Enum refers to an unknown interface: {0}")]
    UnknownInterface(String),

    #[error("Unknown type: {0}")]
    UnknownEnum(String),
}
type Result<T, E = Error> = std::result::Result<T, E>;

/// Mapping from enum names to whether it's a bitflags
/// interface -> name -> bool.
type EnumInfo<'a> = HashMap<&'a str, HashMap<&'a str, bool>>;
fn generate_arg_type_with_lifetime(
    arg: &wl_spec::protocol::Arg,
    lifetime: &Option<Lifetime>,
    iface_version: &HashMap<&str, u32>,
) -> Result<TokenStream> {
    use wl_spec::protocol::Type::*;
    if let Some(enum_) = &arg.enum_ {
        if let Some((iface, name)) = enum_.split_once('.') {
            let version = iface_version
                .get(iface)
                .ok_or(Error::UnknownInterface(enum_.to_string()))?;
            let iface = format_ident!("{}", iface);
            let name = format_ident!("{}", name.to_pascal_case());
            let version = format_ident!("v{}", version);
            Ok(quote! {
                __generated_root::#iface::#version::enums::#name
            })
        } else {
            let name = format_ident!("{}", enum_.to_pascal_case());
            Ok(quote! {
                enums::#name
            })
        }
    } else {
        Ok(match arg.typ {
            Int => quote!(i32),
            Uint => quote!(u32),
            Fixed => quote!(::wl_scanner_support::wayland_types::Fixed),
            Array => {
                if let Some(lifetime) = lifetime {
                    quote!(&#lifetime [u8])
                } else {
                    quote!(Box<[u8]>)
                }
            }
            Fd => quote!(::wl_scanner_support::wayland_types::Fd),
            String => {
                if let Some(lifetime) = lifetime {
                    quote!(::wl_scanner_support::wayland_types::Str<#lifetime>)
                } else {
                    quote!(::wl_scanner_support::wayland_types::String)
                }
            }
            Object => quote!(::wl_scanner_support::wayland_types::Object),
            NewId => quote!(::wl_scanner_support::wayland_types::NewId),
            Destructor => return Err(Error::InvalidArgumentType),
        })
    }
}
fn generate_arg_type(
    arg: &wl_spec::protocol::Arg,
    is_owned: bool,
    iface_version: &HashMap<&str, u32>,
) -> Result<TokenStream> {
    generate_arg_type_with_lifetime(
        arg,
        &(!is_owned).then(|| Lifetime::new("'a", Span::call_site())),
        iface_version,
    )
}

fn type_is_borrowed(ty: &wl_spec::protocol::Type) -> bool {
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

fn generate_serialize_for_type(
    current_iface_name: &str,
    name: &TokenStream,
    arg: &wl_spec::protocol::Arg,
    enum_info: &EnumInfo,
) -> Result<TokenStream> {
    use wl_spec::protocol::Type::*;
    if let Fd = arg.typ {
        return Ok(quote! {
            use ::std::os::unix::io::AsRawFd;
            let fd = #name.as_raw_fd();
            ::wl_scanner_support::ready!(self.writer.as_mut().poll_write_with_fds(cx, &[], &[fd]))?;
        });
    }
    let get = match arg.typ {
        Int | Uint => {
            if let Some(enum_) = &arg.enum_ {
                let is_bitfield = if let Some((iface, enum_)) = enum_.split_once('.') {
                    *enum_info
                        .get(iface)
                        .ok_or_else(|| Error::UnknownInterface(iface.to_string()))?
                        .get(enum_)
                        .ok_or_else(|| Error::UnknownEnum(enum_.to_string()))?
                } else {
                    *enum_info
                        .get(current_iface_name)
                        .unwrap()
                        .get(enum_.as_str())
                        .ok_or_else(|| Error::UnknownEnum(enum_.to_string()))?
                };
                if is_bitfield {
                    quote! {
                        let buf = #name.bits().to_ne_bytes();
                    }
                } else {
                    quote! {
                        let buf: u32 = #name.into();
                        let buf = buf.to_ne_bytes();
                    }
                }
            } else {
                quote! {
                    let buf = #name.to_ne_bytes();
                }
            }
        }
        Fixed | Object | NewId => quote! {
            let buf = #name.0.to_ne_bytes();
        },
        Fd => unreachable!(),
        String => quote! {
            let buf = #name.0.to_bytes_with_nul();
        },
        Array => quote! {
            let buf = #name;
        },
        Destructor => quote! {},
    };
    let fixed_length_write = fixed_length_write();
    Ok(match arg.typ {
        Int | Uint | Fixed | Object | NewId => quote! {
            #get
            #fixed_length_write
        },
        String | Array => quote! {
            #get
            let aligned_len = ((buf.len() + 3) & !3) + 4;
            let align_buf = [0; 3];
            let len_buf = (buf.len() as u32).to_ne_bytes();
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
    })
}
fn generate_message_variant(
    iface_name: &str,
    opcode: u16,
    request: &Message,
    is_owned: bool,
    parent: &Ident,
    iface_version: &HashMap<&str, u32>,
    enum_info: &EnumInfo,
) -> Result<(TokenStream, TokenStream)> {
    let args = &request.args;
    let args: Result<TokenStream> = args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            let ty = generate_arg_type(&arg, is_owned, iface_version)?;
            let attr = if !is_owned && type_is_borrowed(&arg.typ) {
                quote!(#[serde(borrow)])
            } else {
                quote!()
            };
            let doc_comment = generate_doc_comment(&arg.description);
            Ok(quote! {
                #doc_comment
                #attr
                pub #name: #ty,
            })
        })
        .collect();
    let args = args?;
    let pname = request.name.as_str().to_pascal_case();
    let name = format_ident!("{}", pname);
    let is_borrowed = if !is_owned {
        request.args.iter().any(|arg| type_is_borrowed(&arg.typ))
    } else {
        false
    };
    let lifetime = if is_borrowed {
        quote! {
            <'a>
        }
    } else {
        quote! {}
    };
    let serialize_name = format_ident!("{}Serialize", pname);
    let doc_comment = generate_doc_comment(&request.description);
    let public = quote! {
        #doc_comment
        #[derive(::wl_scanner_support::serde::Deserialize, Debug, PartialEq, Eq)]
        #[serde(crate = "::wl_scanner_support::serde")]
        pub struct #name #lifetime {
            #args
        }
        impl #lifetime #name #lifetime {
            pub const OPCODE: u16 = #opcode;
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
    };

    let serialize = generate_message_variant_serialize(iface_name, opcode, request, enum_info)?;
    let private = quote! {
        pub struct #serialize_name <'a, T> {
            #[allow(unused)]
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
    };
    Ok((public, private))
}
fn generate_message_variant_serialize(
    iface_name: &str,
    opcode: u16,
    request: &Message,
    enum_info: &EnumInfo,
) -> Result<TokenStream> {
    let serialize = request
        .args
        .iter()
        .enumerate()
        .map(|(i, arg)| {
            let name = format_ident!("{}", arg.name);
            let name = quote! { self.request.#name };
            let i = i as i32;
            let serialize = generate_serialize_for_type(iface_name, &name, &arg, enum_info)?;
            Ok(quote! {
                #i => {
                    #serialize
                }
            })
        })
        .collect::<Result<TokenStream>>()?;
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
                    + ((self.request.#name.0.to_bytes_with_nul().len() + 3) & !3) as u32
                },
                wl_spec::protocol::Type::Array => quote! {
                    + ((self.request.#name.len() + 3) & !3) as u32
                },
            }
        })
        .collect();
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

fn generate_dispatch_trait(
    messages: &[Message],
    event_or_request: EventOrRequest,
    iface_version: &HashMap<&str, u32>,
) -> Result<TokenStream> {
    let ty = match event_or_request {
        EventOrRequest::Event => format_ident!("EventDispatch"),
        EventOrRequest::Request => format_ident!("RequestDispatch"),
    };
    let methods = messages
        .iter()
        .map(|m| {
            let name = if m.name == "move" || m.name == "type" {
                format_ident!("{}_", m.name)
            } else {
                format_ident!("{}", m.name)
            };
            let args = m
                .args
                .iter()
                .map(|arg| {
                    let name = format_ident!("{}", arg.name);
                    let typ = generate_arg_type_with_lifetime(
                        &arg,
                        &Some(Lifetime::new("'_", Span::call_site())),
                        iface_version,
                    )?;
                    Ok(quote! {
                        #name: #typ
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let doc_comment = generate_doc_comment(&m.description);
            Ok(quote! {
                #doc_comment
                fn #name(
                    ctx: &mut D,
                    client_ctx: &mut C,
                    object: ::wl_scanner_support::wayland_types::Object,
                    #(#args),*
                )
                -> ::std::result::Result<(), Self::Error>;
            })
        })
        .collect::<Result<TokenStream>>()?;
    Ok(quote! {
        pub trait #ty<D, C> {
            type Error;
            #methods
        }
    })
}

fn generate_dispatch_impl(messages: &[Message], event_or_request: EventOrRequest) -> TokenStream {
    let (ty, mod_name) = match event_or_request {
        EventOrRequest::Event => (format_ident!("Event"), format_ident!("events")),
        EventOrRequest::Request => (format_ident!("Request"), format_ident!("requests")),
    };
    let trait_ = match event_or_request {
        EventOrRequest::Event => format_ident!("EventDispatch"),
        EventOrRequest::Request => format_ident!("RequestDispatch"),
    };
    let variants = messages.iter().map(|v| {
        let vname = format_ident!("{}", v.name.to_pascal_case());
        let mname = if v.name == "move" || v.name == "type" {
            format_ident!("{}_", v.name)
        } else {
            format_ident!("{}", v.name)
        };
        let args = v.args.iter().map(|a| format_ident!("{}", a.name));
        let args2 = args.clone();
        quote! {
            #ty::#vname(#mod_name::#vname { #(#args),* }) => {
                Visitor::#mname(ctx, client_ctx, object, #(#args2),*)
            }
        }
    });
    quote! {
        pub fn dispatch<D, C, Visitor: #trait_<D, C>>(
            self,
            ctx: &mut D,
            client_ctx: &mut C,
            object: ::wl_scanner_support::wayland_types::Object
        ) -> Result<(), Visitor::Error> {
            match self {
                #(#variants),*
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EventOrRequest {
    Event,
    Request,
}

fn generate_event_or_request(
    iface_name: &str,
    messages: &[Message],
    iface_version: &HashMap<&str, u32>,
    enum_info: &EnumInfo,
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
            .map(|(opcode, v)| {
                generate_message_variant(
                    iface_name,
                    opcode as u16,
                    v,
                    false,
                    &mod_name,
                    iface_version,
                    enum_info,
                )
            })
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
                let attr = if is_borrowed {
                    quote! { #[serde(borrow)] }
                } else {
                    quote! {}
                };
                quote! {
                    #attr
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
        let dispatch = generate_dispatch_trait(messages, event_or_request, iface_version)?;
        let dispatch_impl = generate_dispatch_impl(messages, event_or_request);
        let public = quote! {
            pub mod #mod_name {
                #[allow(unused_imports)]
                use super::enums;
                #[allow(unused_imports)]
                use super::__generated_root;
                #public
            }
            #[doc = "Collection of all possible types of messages, see individual message types "]
            #[doc = "for more information."]
            #[derive(Debug, PartialEq, Eq, ::wl_scanner_support::serde::Deserialize)]
            #[serde(crate = "::wl_scanner_support::serde")]
            pub enum #type_name #enum_lifetime {
                #enum_members
            }
            #dispatch
            impl #enum_lifetime #type_name #enum_lifetime {
                #dispatch_impl
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
        private.extend(quote! {
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
fn wrap_links(line: &str) -> String {
    let links = linkify::LinkFinder::new().links(line);
    let mut result = String::new();
    let mut curr_pos = 0;
    for link in links {
        if link.start() > curr_pos {
            result.push_str(&line[curr_pos..link.start()]);
        }
        result.push_str("<");
        result.push_str(link.as_str());
        result.push_str(">");
        curr_pos = link.end();
    }
    result.push_str(&line[curr_pos..]);
    result
}

use lazy_static::lazy_static;
use regex::{Captures, Regex};
lazy_static! {
    static ref LINKREF_REGEX: Regex = Regex::new(r"\[([0-9]+)\]").unwrap();
    static ref LINKREF_REGEX_WITH_SPACE: Regex = Regex::new(r"\s*\[([0-9]+)\]").unwrap();
}
fn generate_doc_comment(description: &Option<(String, String)>) -> TokenStream {
    if let Some((summary, desc)) = description {
        let link_refs: HashMap<u32, &str> = desc
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                LINKREF_REGEX
                    .captures(line)
                    .and_then(|m| {
                        if m.get(0).unwrap().start() != 0 {
                            None
                        } else {
                            Some(m)
                        }
                    })
                    .and_then(|m| m.get(1))
                    .and_then(|refcap| {
                        if let Ok(refnum) = refcap.as_str().parse::<u32>() {
                            Some((refnum, line[refcap.end() + 1..].trim()))
                        } else {
                            None
                        }
                    })
            })
            .collect();
        let desc = desc
            .split("\n")
            .filter_map(|s| {
                let s = s.trim();
                if let Some(m) = LINKREF_REGEX.find(s) {
                    if m.start() == 0 {
                        return None;
                    }
                }
                let mut s = wrap_links(s);
                s = LINKREF_REGEX_WITH_SPACE
                    .replace(&s, |m: &Captures| {
                        let refnum = m.get(1).unwrap().as_str().parse::<u32>().unwrap();
                        let link = link_refs.get(&refnum).unwrap();
                        format!(
                            "<a href={link}><sup>{refnum}</sup></a>",
                            refnum = refnum,
                            link = link
                        )
                    })
                    .into_owned();
                s = s.replace('[', "\\[").replace(']', "\\]");
                Some(quote! {
                    #[doc = #s]
                })
            })
            .collect::<TokenStream>();
        let summary = summary.replace('[', "\\[").replace(']', "\\]");
        quote! {
            #[doc = #summary]
            #[doc = ""]
            #desc
        }
    } else {
        quote! {}
    }
}
fn generate_enums(enums: &[Enum]) -> TokenStream {
    let enums = enums
        .iter()
        .map(|e| {
            let doc = generate_doc_comment(&e.description);
            let name = format_ident!("{}", e.name.to_pascal_case());
            let is_bitfield = e.bitfield;
            let members = e.entries.iter().map(|e| {
                let name = if e.name.chars().all(|x| x.is_ascii_digit()) {
                    format_ident!("_{}", e.name)
                } else {
                    if is_bitfield {
                        format_ident!("{}", e.name.TO_SHOUTY_SNEK_CASE())
                    } else {
                        format_ident!("{}", e.name.to_pascal_case())
                    }
                };
                let value = e.value;
                let summary = e.summary.as_deref().unwrap_or("");
                let summary = summary.replace('[', "\\[").replace(']', "\\]");
                if is_bitfield {
                    quote! {
                        #[doc = #summary]
                        const #name = #value;
                    }
                } else {
                    quote! {
                        #[doc = #summary]
                        #name = #value,
                    }
                }
            });
            if is_bitfield {
                quote! {
                    ::wl_scanner_support::bitflags! {
                        #doc
                        #[derive(
                            ::wl_scanner_support::serde::Serialize,
                            ::wl_scanner_support::serde::Deserialize,
                        )]
                        #[serde(crate = "::wl_scanner_support::serde")]
                        pub struct #name: u32 {
                            #(#members)*
                        }
                    }
                }
            } else {
                quote! {
                    #doc
                    #[derive(
                        ::wl_scanner_support::num_enum::IntoPrimitive,
                        ::wl_scanner_support::num_enum::TryFromPrimitive,
                        Debug, Clone, Copy, PartialEq, Eq
                    )]
                    #[repr(u32)]
                    pub enum #name {
                        #(#members)*
                    }
                    impl ::wl_scanner_support::serde::Serialize for #name {
                        fn serialize<S: ::wl_scanner_support::serde::Serializer>(&self, serializer: S)
                        -> ::std::result::Result<S::Ok, S::Error> {
                            let num: u32 = (*self).into();
                            serializer.serialize_u32(num)
                        }
                    }
                    impl<'de> ::wl_scanner_support::serde::Deserialize<'de> for #name {
                        fn deserialize<D: ::wl_scanner_support::serde::Deserializer<'de>>(deserializer: D)
                        -> ::std::result::Result<Self, D::Error> {
                            use ::wl_scanner_support::serde::de::Error;
                            let value = u32::deserialize(deserializer)?;
                            Self::try_from(value)
                                .map_err(|_| D::Error::custom("invalid enum value"))
                        }
                    }
                }
            }
        });
    quote! {
        pub mod enums {
            #(#enums)*
        }
    }
}
fn generate_interface(
    iface: &Interface,
    iface_version: &HashMap<&str, u32>,
    enum_info: &EnumInfo,
) -> Result<TokenStream> {
    let name = format_ident!("{}", iface.name);
    let version = format_ident!("v{}", iface.version);

    let (requests, requests_private) = generate_event_or_request(
        &iface.name,
        &iface.requests,
        iface_version,
        enum_info,
        EventOrRequest::Request,
    )?;
    let (events, events_private) = generate_event_or_request(
        &iface.name,
        &iface.events,
        iface_version,
        enum_info,
        EventOrRequest::Event,
    )?;
    let doc_comment = generate_doc_comment(&iface.description);
    let enums = generate_enums(&iface.enums);

    Ok(quote! {
        #doc_comment
        pub mod #name {
            pub mod #version {
                #[allow(unused_imports)]
                use super::super::__generated_root;
                #requests
                #events
                #enums
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
    let iface_version = proto
        .interfaces
        .iter()
        .map(|i| (i.name.as_str(), i.version))
        .collect();
    let enum_info: EnumInfo = proto
        .interfaces
        .iter()
        .map(|i| {
            (
                i.name.as_str(),
                i.enums
                    .iter()
                    .map(move |e| (e.name.as_str(), e.bitfield))
                    .collect(),
            )
        })
        .collect();
    let interfaces = proto
        .interfaces
        .iter()
        .map(|v| generate_interface(v, &iface_version, &enum_info))
        .collect::<Result<TokenStream>>()?;
    let name = format_ident!("{}", proto.name);
    Ok(quote! {
        pub mod #name {
            use super::#name as __generated_root;
            #interfaces
        }
    })
}

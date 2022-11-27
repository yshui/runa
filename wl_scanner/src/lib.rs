use ahash::AHashMap as HashMap;

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

    #[error("Cannot decide type of enum: {0}")]
    BadEnumType(String),
}
type Result<T, E = Error> = std::result::Result<T, E>;

struct EnumInfo {
    is_bitfield: bool,
    is_int:      Option<bool>,
}
/// Mapping from enum names to whether it's a bitflags
/// interface -> name -> bool.
struct EnumInfos<'a>(HashMap<&'a str, HashMap<&'a str, EnumInfo>>);
impl<'a> EnumInfos<'a> {
    fn get(&self, current_iface_name: &str, enum_: &str) -> Result<&EnumInfo, Error> {
        Ok(if let Some((iface, enum_)) = enum_.split_once('.') {
            self.0
                .get(iface)
                .ok_or_else(|| Error::UnknownInterface(iface.to_string()))?
                .get(enum_)
                .ok_or_else(|| Error::UnknownEnum(enum_.to_string()))?
        } else {
            self.0
                .get(current_iface_name)
                .unwrap()
                .get(enum_)
                .ok_or_else(|| Error::UnknownEnum(enum_.to_string()))?
        })
    }

    fn get_mut(&mut self, current_iface_name: &str, enum_: &str) -> Result<&mut EnumInfo, Error> {
        Ok(if let Some((iface, enum_)) = enum_.split_once('.') {
            self.0
                .get_mut(iface)
                .ok_or_else(|| Error::UnknownInterface(iface.to_string()))?
                .get_mut(enum_)
                .ok_or_else(|| Error::UnknownEnum(enum_.to_string()))?
        } else {
            self.0
                .get_mut(current_iface_name)
                .unwrap()
                .get_mut(enum_)
                .ok_or_else(|| Error::UnknownEnum(enum_.to_string()))?
        })
    }
}
fn to_path<'a>(arr: impl IntoIterator<Item = &'a str>, leading_colon: bool) -> syn::Path {
    syn::Path {
        leading_colon: if leading_colon {
            Some(Default::default())
        } else {
            None
        },
        segments:      arr
            .into_iter()
            .map(|s| syn::PathSegment::from(syn::Ident::new(s, Span::call_site())))
            .collect(),
    }
}
macro_rules! path {
    ($($seg:ident)::*) => {
        to_path([ $( stringify!($seg) ),* ], false)
    };
    (::$($seg:ident)::*) => {
        to_path([ $( stringify!($seg) ),* ], true)
    };
}
macro_rules! type_path {
    ($($seg:ident)::*) => {
        syn::Type::Path(syn::TypePath {
            qself: None,
            path: to_path([ $( stringify!($seg) ),* ], false),
        })
    };
    (::$($seg:ident)::*) => {
        syn::Type::Path(syn::TypePath {
            qself: None,
            path: to_path([ $( stringify!($seg) ),* ], true),
        })
    };
}
fn enum_type_name(enum_: &str, iface_version: &HashMap<&str, u32>) -> syn::Path {
    if let Some((iface, name)) = enum_.split_once('.') {
        let version = iface_version.get(iface).unwrap();
        to_path(
            [
                "__generated_root",
                iface,
                &format!("v{}", version),
                "enums",
                &name.to_pascal_case(),
            ],
            false,
        )
    } else {
        to_path(["enums", &enum_.to_pascal_case()], false)
    }
}
fn generate_arg_type_with_lifetime(
    arg: &wl_spec::protocol::Arg,
    lifetime: &Option<Lifetime>,
    iface_version: &HashMap<&str, u32>,
) -> syn::Type {
    use wl_spec::protocol::Type::*;
    if let Some(enum_) = &arg.enum_ {
        syn::Type::Path(syn::TypePath {
            path:  enum_type_name(enum_, iface_version),
            qself: None,
        })
    } else {
        match arg.typ {
            Int => type_path!(i32),
            Uint => type_path!(u32),
            Fixed => type_path!(::wl_scanner_support::wayland_types::Fixed),
            Array =>
                if let Some(lifetime) = lifetime {
                    // &#lifetime [u8]
                    syn::Type::Reference(syn::TypeReference {
                        and_token:  Default::default(),
                        lifetime:   Some(lifetime.clone()),
                        mutability: None,
                        elem:       Box::new(syn::Type::Slice(syn::TypeSlice {
                            bracket_token: Default::default(),
                            elem:          Box::new(type_path!(u8)),
                        })),
                    })
                } else {
                    // Box<[u8]>
                    syn::Type::Path(syn::TypePath {
                        qself: None,
                        path: syn::Path {
                            leading_colon: None,
                            segments: [syn::PathSegment {
                                ident:     syn::Ident::new("Box", Span::call_site()),
                                arguments: syn::PathArguments::AngleBracketed(
                                    syn::AngleBracketedGenericArguments {
                                        colon2_token: None,
                                        lt_token:     Default::default(),
                                        args:         [syn::GenericArgument::Type(
                                            syn::Type::Slice(syn::TypeSlice {
                                                bracket_token: Default::default(),
                                                elem:          Box::new(type_path!(u8)),
                                            }),
                                        )]
                                        .into_iter()
                                        .collect(),
                                        gt_token:     Default::default(),
                                    },
                                ),
                            }]
                            .into_iter()
                            .collect(),
                        },
                    })
                },
            Fd => type_path!(::wl_scanner_support::wayland_types::Fd),
            String =>
                if let Some(lifetime) = lifetime {
                    // Str<#lifetime>
                    let mut ty = path!(::wl_scanner_support::wayland_types::Str);
                    ty.segments.last_mut().unwrap().arguments =
                        syn::PathArguments::AngleBracketed(syn::AngleBracketedGenericArguments {
                            colon2_token: None,
                            lt_token:     Default::default(),
                            args:         [syn::GenericArgument::Lifetime(lifetime.clone())]
                                .into_iter()
                                .collect(),
                            gt_token:     Default::default(),
                        });
                    syn::Type::Path(syn::TypePath {
                        path:  ty,
                        qself: None,
                    })
                } else {
                    type_path!(::wl_scanner_support::wayland_types::String)
                },
            Object => type_path!(::wl_scanner_support::wayland_types::Object),
            NewId => type_path!(::wl_scanner_support::wayland_types::NewId),
            Destructor => panic!("InvalidArgumentType"),
        }
    }
}
fn generate_arg_type(
    arg: &wl_spec::protocol::Arg,
    is_owned: bool,
    iface_version: &HashMap<&str, u32>,
) -> syn::Type {
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

fn generate_serialize_for_type(
    current_iface_name: &str,
    name: &syn::Ident,
    arg: &wl_spec::protocol::Arg,
    enum_info: &EnumInfos,
) -> TokenStream {
    use wl_spec::protocol::Type::*;
    if let Fd = arg.typ {
        return quote! {
            writer
                .as_mut()
                .push_fds(&mut ::std::iter::once(self.#name.take().expect("trying to send raw fd")));
        }
    }
    let get = match arg.typ {
        Int | Uint =>
            if let Some(enum_) = &arg.enum_ {
                let info = enum_info.get(current_iface_name, enum_).unwrap();
                let repr = if info.is_int.unwrap_or(false) {
                    path!(i32)
                } else {
                    path!(u32)
                };
                if info.is_bitfield {
                    quote! {
                        let buf = self.#name.bits().to_ne_bytes();
                    }
                } else {
                    quote! {
                        let buf: #repr = self.#name.into();
                        let buf = buf.to_ne_bytes();
                    }
                }
            } else {
                quote! {
                    let buf = self.#name.to_ne_bytes();
                }
            },
        Fixed | Object | NewId => quote! {
            let buf = self.#name.0.to_ne_bytes();
        },
        Fd => unreachable!(),
        String => quote! {
            let buf = self.#name.0.to_bytes_with_nul();
        },
        Array => quote! {
            let buf = self.#name;
        },
        Destructor => quote! {},
    };
    match arg.typ {
        Int | Uint | Fixed | Object | NewId => quote! {
            #get
            writer.as_mut().write(&buf);
        },
        String | Array => quote! {
            #get
            // buf aligned to 4 bytes, plus length prefix
            let aligned_len = ((buf.len() + 3) & !3) + 4;
            let align_buf = [0; 3];
            let len_buf = (buf.len() as u32).to_ne_bytes();
            // [0, 4): length
            // [4, buf.len()+4): buf
            // [buf.len()+4, aligned_len): alignment
            writer.as_mut().write(&len_buf);
            writer.as_mut().write(buf);
            writer.as_mut().write(&align_buf[..(aligned_len - buf.len() - 4)]);
        },
        Fd => unreachable!(),
        Destructor => quote! {},
    }
}
fn generate_deserialize_for_type(
    current_iface_name: &str,
    arg: &wl_spec::protocol::Arg,
    enum_info: &EnumInfos,
    iface_version: &HashMap<&str, u32>,
) -> TokenStream {
    use wl_spec::protocol::Type::*;
    match arg.typ {
        Int | Uint => {
            let v = if arg.typ == Int {
                quote! {  pop_i32(&mut data) }
            } else {
                quote! { pop_u32(&mut data) }
            };
            let err = if arg.typ == Int {
                quote! { ::wl_scanner_support::io::de::Error::InvalidIntEnum }
            } else {
                quote! { ::wl_scanner_support::io::de::Error::InvalidUintEnum }
            };
            if let Some(enum_) = &arg.enum_ {
                let is_bitfield = enum_info
                    .get(current_iface_name, enum_)
                    .unwrap()
                    .is_bitfield;
                let enum_ty = enum_type_name(enum_, iface_version);
                let enum_ty = quote! { super::#enum_ty };
                if is_bitfield {
                    quote! { {
                        let tmp = #v;
                        #enum_ty::from_bits(tmp).ok_or_else(|| #err(tmp, std::any::type_name::<#enum_ty>()))?
                    } }
                } else {
                    quote! { {
                        let tmp = #v;
                        #enum_ty::try_from(tmp).map_err(|_| #err(tmp, std::any::type_name::<#enum_ty>()))?
                    } }
                }
            } else {
                v
            }
        },
        Fixed => quote! { pop_i32(&mut data).into() },
        Object | NewId => quote! { pop_u32(&mut data).into() },
        Fd => quote! { pop_fd(&mut fds).into() },
        String => quote! { {
            let len = pop_u32(&mut data);
            pop_bytes(&mut data, len as usize).into()
        } },
        Array => quote! { {
            let len = pop_u32(&mut data);
            pop_bytes(&mut data, len as usize)
        } },
        Destructor => quote! {},
    }
}
fn generate_message_variant(
    iface_name: &str,
    opcode: u16,
    request: &Message,
    is_owned: bool,
    _parent: &Ident,
    iface_version: &HashMap<&str, u32>,
    enum_info: &EnumInfos,
) -> TokenStream {
    let args = &request.args;
    let args = args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            let ty = generate_arg_type(arg, is_owned, iface_version);
            let doc_comment = generate_doc_comment(&arg.description);
            quote! {
                #doc_comment
                pub #name: #ty
            }
        });
    let pname = request.name.as_str().to_pascal_case();
    let name = format_ident!("{}", pname);
    let is_borrowed = if !is_owned {
        request.args.iter().any(|arg| type_is_borrowed(&arg.typ))
    } else {
        false
    };
    let empty = quote! {};
    let lta = quote!(<'a>);
    let lifetime = if is_borrowed { &lta } else { &empty };
    // Generate experssion for length calculation
    let fixed_len: u32 = request
        .args
        .iter()
        .filter(|arg| match arg.typ {
            | wl_spec::protocol::Type::Int
            | wl_spec::protocol::Type::Uint
            | wl_spec::protocol::Type::Fixed
            | wl_spec::protocol::Type::Object
            | wl_spec::protocol::Type::String // string and array has a length prefix, so they
            | wl_spec::protocol::Type::Array  // count, too.
            | wl_spec::protocol::Type::NewId => true,
            | wl_spec::protocol::Type::Fd
            | wl_spec::protocol::Type::Destructor=> false,
        })
        .count() as u32 *
        4 + 8; // 8 bytes for header
    let variable_len = request.args.iter().map(|arg| {
        let name = format_ident!("{}", arg.name);
        match &arg.typ {
            wl_spec::protocol::Type::Int |
            wl_spec::protocol::Type::Uint |
            wl_spec::protocol::Type::Fixed |
            wl_spec::protocol::Type::Object |
            wl_spec::protocol::Type::NewId |
            wl_spec::protocol::Type::Fd |
            wl_spec::protocol::Type::Destructor => quote! {},
            wl_spec::protocol::Type::String => quote! {
                + ((self.#name.0.to_bytes_with_nul().len() + 3) & !3) as u32
            },
            wl_spec::protocol::Type::Array => quote! {
                + ((self.#name.len() + 3) & !3) as u32
            },
        }
    });
    let serialize = request
        .args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            generate_serialize_for_type(iface_name, &name, arg, enum_info)
        });
    let deserialize = request
        .args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            let deserialize =
                generate_deserialize_for_type(iface_name, arg, enum_info, iface_version);
            quote! {
                #name: #deserialize
            }
        });
    let doc_comment = generate_doc_comment(&request.description);
    let nfds: u8 = request
        .args
        .iter()
        .filter(|arg| arg.typ == wl_spec::protocol::Type::Fd)
        .count()
        .try_into()
        .unwrap();
    let mut_ = if nfds != 0 {
        quote! {mut}
    } else {
        quote! {}
    };
    let public = quote! {
        #doc_comment
        #[derive(Debug, PartialEq, Eq)]
        pub struct #name #lifetime {
            #(#args),*
        }
        impl #lifetime #name #lifetime {
            pub const OPCODE: u16 = #opcode;
        }
        impl #lifetime ::wl_scanner_support::io::ser::Serialize for #name #lifetime {
            fn serialize<W: ::wl_scanner_support::io::buf::AsyncBufWriteWithFd>(
                #mut_ self,
                mut writer: ::std::pin::Pin<&mut W>
            ) {
                let msg_len = self.len() as u32;
                let prefix: u32 = (msg_len << 16) + (#opcode as u32);
                writer.as_mut().write(&prefix.to_ne_bytes());
                #(#serialize)*
            }
            #[inline]
            fn len(&self) -> u16 {
                (#fixed_len #(#variable_len)*) as u16
            }
            #[inline]
            fn nfds(&self) -> u8 {
                #nfds
            }
        }
        impl<'a> ::wl_scanner_support::io::de::Deserialize<'a> for #name #lifetime {
            #[inline]
            fn deserialize(
                mut data: &'a [u8], mut fds: &'a [::std::os::unix::io::RawFd] 
            ) -> Result<Self, ::wl_scanner_support::io::de::Error> {
                use ::wl_scanner_support::io::{pop_fd, pop_bytes, pop_i32, pop_u32};
                Ok(Self {
                    #(#deserialize),*
                })
            }
        }
    };

    public
}

fn generate_dispatch_trait(
    messages: &[Message],
    event_or_request: EventOrRequest,
    iface_version: &HashMap<&str, u32>,
) -> TokenStream {
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
            let retty = format_ident!("{}Fut", m.name.to_pascal_case());
            let args = m
                .args
                .iter()
                .map(|arg| {
                    let name = format_ident!("{}", arg.name);
                    let typ = generate_arg_type(arg, false, iface_version);
                    quote! {
                        #name: #typ
                    }
                });
            let doc_comment = generate_doc_comment(&m.description);
            quote! {
                #doc_comment
                fn #name<'a>(
                    &'a self,
                    ctx: &'a mut Ctx,
                    object_id: u32,
                    #(#args),*
                )
                -> Self::#retty<'a>;
            }
        });
    let futs = messages
        .iter()
        .map(|m| format_ident!("{}Fut", m.name.to_pascal_case()));
    quote! {
        pub trait #ty<Ctx> {
            type Error: ::wl_scanner_support::ProtocolError;
            #(
                type #futs<'a>: ::std::future::Future<Output = ::std::result::Result<(), Self::Error>> + 'a
                    where Ctx: 'a, Self: 'a;
            )*
            #(#methods)*
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
    enum_info: &EnumInfos,
    event_or_request: EventOrRequest,
) -> TokenStream {
    if messages.is_empty() {
        quote! {}
    } else {
        let mod_name = match event_or_request {
            EventOrRequest::Event => format_ident!("events"),
            EventOrRequest::Request => format_ident!("requests"),
        };
        let type_name = match event_or_request {
            EventOrRequest::Event => format_ident!("Event"),
            EventOrRequest::Request => format_ident!("Request"),
        };
        let enum_is_borrowed = messages
            .iter()
            .any(|v| v.args.iter().any(|arg| type_is_borrowed(&arg.typ)));
        let public = messages
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
            });
        let enum_lifetime = if enum_is_borrowed {
            quote! { <'a> }
        } else {
            quote! {}
        };
        let enum_members = messages.iter().map(|v| {
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
        });

        let enum_serialize_cases = messages.iter().map(|v| {
            let name = format_ident!("{}", v.name.to_pascal_case());
            quote! {
                Self::#name(v) => v.serialize(writer),
            }
        });
        let enum_deserialize_cases = messages
            .iter()
            .enumerate()
            .map(|(opcode, v)| {
                let name = format_ident!("{}", v.name.to_pascal_case());
                let opcode = opcode as u32;
                quote! {
                    #opcode => {
                        Ok(Self::#name(<#mod_name::#name>::deserialize(data, fds)?))
                    },
                }
            });
        let enum_len_cases = messages.iter().map(|v| {
            let name = format_ident!("{}", v.name.to_pascal_case());
            quote! {
                Self::#name(v) => v.len(),
            }
        });
        let enum_nfds_cases = messages.iter().map(|v| {
            let name = format_ident!("{}", v.name.to_pascal_case());
            quote! {
                Self::#name(v) => v.nfds(),
            }
        });
        let dispatch = generate_dispatch_trait(messages, event_or_request, iface_version);
        let public = quote! {
            pub mod #mod_name {
                use super::enums;
                use super::__generated_root;
                #(#public)*
            }
            #[doc = "Collection of all possible types of messages, see individual message types "]
            #[doc = "for more information."]
            #[derive(Debug, PartialEq, Eq)]
            pub enum #type_name #enum_lifetime {
                #(#enum_members)*
            }
            #dispatch
            impl #enum_lifetime ::wl_scanner_support::io::ser::Serialize for #type_name #enum_lifetime {
                fn serialize<W: ::wl_scanner_support::io::buf::AsyncBufWriteWithFd>(
                    self,
                    writer: ::std::pin::Pin<&mut W>
                ) {
                    match self {
                        #(#enum_serialize_cases)*
                    }
                }
                #[inline]
                fn len(&self) -> u16 {
                    match self {
                        #(#enum_len_cases)*
                    }
                }
                #[inline]
                fn nfds(&self) -> u8 {
                    match self {
                        #(#enum_nfds_cases)*
                    }
                }
            }
            impl<'a> ::wl_scanner_support::io::de::Deserialize<'a> for #type_name #enum_lifetime {
                fn deserialize(
                    mut data: &'a [u8], mut fds: &'a [::std::os::unix::io::RawFd]
                ) -> ::std::result::Result<Self, ::wl_scanner_support::io::de::Error> {
                    use ::wl_scanner_support::io::pop_u32;
                    let _object_id = pop_u32(&mut data);
                    let header = pop_u32(&mut data);
                    let opcode = header & 0xFFFF;
                    match opcode {
                        #(#enum_deserialize_cases)*
                        _ => Err(
                            ::wl_scanner_support::io::de::Error::UnknownOpcode(
                                opcode, std::any::type_name::<Self>())),
                    }
                }
            }
        };
        public
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
        result.push('<');
        result.push_str(link.as_str());
        result.push('>');
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
                        return None
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
            });
        let summary = summary.replace('[', "\\[").replace(']', "\\]");
        quote! {
            #[doc = #summary]
            #[doc = ""]
            #(#desc)*
        }
    } else {
        quote! {}
    }
}
fn generate_enums(enums: &[Enum], current_iface_name: &str, enum_info: &EnumInfos) -> TokenStream {
    let enums = enums.iter().map(|e| {
        let doc = generate_doc_comment(&e.description);
        let name = format_ident!("{}", e.name.to_pascal_case());
        let info = enum_info.get(current_iface_name, &e.name).unwrap();
        let is_bitfield = e.bitfield;
        assert_eq!(info.is_bitfield, is_bitfield);
        let repr = if info.is_int.unwrap_or(false) {
            quote! { i32 }
        } else {
            quote! { u32 }
        };
        let members = e.entries.iter().map(|e| {
            let name = if e.name.chars().all(|x| x.is_ascii_digit()) {
                format_ident!("_{}", e.name)
            } else if is_bitfield {
                format_ident!("{}", e.name.TO_SHOUTY_SNEK_CASE())
            } else {
                format_ident!("{}", e.name.to_pascal_case())
            };
            let value = if info.is_int.unwrap_or(false) {
                let value = e.value as i32;
                quote! { #value }
            } else {
                let value = e.value;
                quote! { #value }
            };
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
                    pub struct #name: #repr {
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
                #[repr(#repr)]
                pub enum #name {
                    #(#members)*
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
    enum_info: &EnumInfos,
) -> TokenStream {
    let name = format_ident!("{}", iface.name);
    let version = format_ident!("v{}", iface.version);

    let requests = generate_event_or_request(
        &iface.name,
        &iface.requests,
        iface_version,
        enum_info,
        EventOrRequest::Request,
    );
    let events = generate_event_or_request(
        &iface.name,
        &iface.events,
        iface_version,
        enum_info,
        EventOrRequest::Event,
    );
    let doc_comment = generate_doc_comment(&iface.description);
    let enums = generate_enums(&iface.enums, &iface.name, enum_info);

    let iface_name = &iface.name;
    let iface_version = iface.version;
    quote! {
        #doc_comment
        pub mod #name {
            #![allow(unused_imports, unused_mut, unused_variables)]
            pub mod #version {
                use super::super::__generated_root;
                /// Name of the interface
                pub const NAME: &str = #iface_name;
                pub const VERSION: u32 = #iface_version;
                #requests
                #events
                #enums
            }
        }
    }
}

fn scan_enum(proto: &Protocol, enum_info: &mut EnumInfos) -> Result<()> {
    for iface in proto.interfaces.iter() {
        for req in iface.requests.iter().chain(iface.events.iter()) {
            for arg in req.args.iter() {
                if let Some(ref enum_) = arg.enum_ {
                    let info = enum_info.get_mut(&iface.name, enum_)?;
                    let is_int = arg.typ == wl_spec::protocol::Type::Int;

                    eprintln!("{}::{}: is_int={}", iface.name, enum_, is_int);
                    if let Some(old) = info.is_int {
                        if old != is_int {
                            return Err(Error::BadEnumType(enum_.clone()))
                        }
                    } else {
                        info.is_int = Some(is_int);
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn generate_protocol(proto: &Protocol) -> Result<TokenStream> {
    let iface_version = proto
        .interfaces
        .iter()
        .map(|i| (i.name.as_str(), i.version))
        .collect();
    let mut enum_info = EnumInfos(
        proto
            .interfaces
            .iter()
            .map(|i| {
                (
                    i.name.as_str(),
                    i.enums
                        .iter()
                        .map(move |e| {
                            (e.name.as_str(), EnumInfo {
                                is_bitfield: e.bitfield,
                                is_int:      None,
                            })
                        })
                        .collect(),
                )
            })
            .collect(),
    );
    scan_enum(proto, &mut enum_info)?;
    let interfaces = proto
        .interfaces
        .iter()
        .map(|v| generate_interface(v, &iface_version, &enum_info));
    let name = format_ident!("{}", proto.name);
    Ok(quote! {
        pub mod #name {
            use super::#name as __generated_root;
            #(#interfaces)*
        }
    })
}

use std::borrow::Cow;

use ahash::AHashMap as HashMap;
use heck::{ToPascalCase, ToShoutySnekCase};
use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};
use syn::Lifetime;
use thiserror::Error;
use spec_parser::protocol::{Enum, Interface, Message, Protocol};
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
                &format!("v{version}"),
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
    arg: &spec_parser::protocol::Arg,
    lifetime: &Option<Lifetime>,
    iface_version: &HashMap<&str, u32>,
) -> syn::Type {
    use spec_parser::protocol::Type::*;
    if let Some(enum_) = &arg.enum_ {
        syn::Type::Path(syn::TypePath {
            path:  enum_type_name(enum_, iface_version),
            qself: None,
        })
    } else {
        match arg.typ {
            Int => type_path!(i32),
            Uint => type_path!(u32),
            Fixed => type_path!(::runa_wayland_scanner::types::Fixed),
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
                        path:  syn::Path {
                            leading_colon: None,
                            segments:      [syn::PathSegment {
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
            Fd => type_path!(::runa_wayland_scanner::types::Fd),
            String =>
                if let Some(lifetime) = lifetime {
                    // Str<#lifetime>
                    let mut ty = path!(::runa_wayland_scanner::types::Str);
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
                    type_path!(::runa_wayland_scanner::types::String)
                },
            Object => type_path!(::runa_wayland_scanner::types::Object),
            NewId => type_path!(::runa_wayland_scanner::types::NewId),
            Destructor => panic!("InvalidArgumentType"),
        }
    }
}
fn generate_arg_type(
    arg: &spec_parser::protocol::Arg,
    is_owned: bool,
    iface_version: &HashMap<&str, u32>,
) -> syn::Type {
    generate_arg_type_with_lifetime(
        arg,
        &(!is_owned).then(|| Lifetime::new("'a", Span::call_site())),
        iface_version,
    )
}

fn type_is_borrowed(ty: &spec_parser::protocol::Type) -> bool {
    use spec_parser::protocol::Type::*;
    match ty {
        Int | Uint | Fixed | Fd | Object | NewId => false,
        String | Array => true,
        Destructor => false,
    }
}

fn generate_serialize_for_type(
    current_iface_name: &str,
    name: &syn::Ident,
    arg: &spec_parser::protocol::Arg,
    enum_info: &EnumInfos,
) -> TokenStream {
    use spec_parser::protocol::Type::*;
    if let Fd = arg.typ {
        return quote! {
            fds.extend(Some(self.#name.take().expect("trying to send raw fd")));
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
                        &self.#name.bits().to_ne_bytes()[..]
                    }
                } else {
                    quote! {
                        {
                            let b: #repr = self.#name.into();
                            &b.to_ne_bytes()[..]
                        }
                    }
                }
            } else {
                quote! {
                    &self.#name.to_ne_bytes()[..]
                }
            },
        Fixed | Object | NewId => quote! {
            &self.#name.0.to_ne_bytes()[..]
        },
        Fd => unreachable!(),
        String => quote! {
            self.#name.0
        },
        Array => quote! {
            self.#name
        },
        Destructor => quote! {},
    };
    match arg.typ {
        Int | Uint | Fixed | Object | NewId => quote! {
            buf.put_slice(#get);
        },
        Array => quote! {
            let tmp = #get;
            // buf aligned to 4 bytes, plus length prefix
            let aligned_len = ((tmp.len() + 3) & !3) + 4;
            // [0, 4): length
            // [4, buf.len()+4): buf
            // [buf.len()+4, aligned_len): alignment
            buf.put_u32_ne(tmp.len() as u32);
            buf.put_slice(tmp);
            buf.put_bytes(0, aligned_len - tmp.len() - 4);
        },
        String => quote! {
            let tmp = #get;
            // tmp doesn't have the trailing nul byte, so we add 1 to obtain its real length
            let aligned_len = ((tmp.len() + 1 + 3) & !3) + 4;
            // [0, 4): length
            // [4, buf.len()+4): buf
            // [buf.len()+4, buf.len()+1+4): nul        -- written together
            // [buf.len()+1+4, aligned_len): alignment  -╯
            buf.put_u32_ne((tmp.len() + 1) as u32);
            buf.put_slice(tmp);
            buf.put_bytes(0, aligned_len - tmp.len() - 4);
        },
        Fd => unreachable!(),
        Destructor => quote! {},
    }
}
fn generate_deserialize_for_type(
    current_iface_name: &str,
    arg_name: &str,
    arg: &spec_parser::protocol::Arg,
    enum_info: &EnumInfos,
    iface_version: &HashMap<&str, u32>,
) -> TokenStream {
    use spec_parser::protocol::Type::*;
    match arg.typ {
        Int | Uint => {
            let v = if arg.typ == Int {
                quote! {  pop_i32(&mut data) }
            } else {
                quote! { pop_u32(&mut data) }
            };
            let err = if arg.typ == Int {
                quote! { ::runa_wayland_scanner::io::de::Error::InvalidIntEnum }
            } else {
                quote! { ::runa_wayland_scanner::io::de::Error::InvalidUintEnum }
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
        Fixed => quote! { ::runa_wayland_scanner::types::Fixed::from_bits(pop_i32(&mut data)) },
        Object | NewId => quote! { pop_u32(&mut data).into() },
        Fd => quote! { pop_fd(&mut fds).into() },
        String => quote! { {
            let len = pop_u32(&mut data) as usize;
            let bytes = pop_bytes(&mut data, len);
            if bytes[len - 1] != b'\0' {
                return Err(::runa_wayland_scanner::io::de::Error::MissingNul(#arg_name));
            }
            bytes[..len - 1].into()
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
    let args = args.iter().map(|arg| {
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
            | spec_parser::protocol::Type::Int
            | spec_parser::protocol::Type::Uint
            | spec_parser::protocol::Type::Fixed
            | spec_parser::protocol::Type::Object
            | spec_parser::protocol::Type::String // string and array has a length prefix, so they
            | spec_parser::protocol::Type::Array  // count, too.
            | spec_parser::protocol::Type::NewId => true,
            | spec_parser::protocol::Type::Fd
            | spec_parser::protocol::Type::Destructor=> false,
        })
        .count() as u32 *
        4 +
        8; // 8 bytes for header
    let variable_len = request.args.iter().map(|arg| {
        let name = format_ident!("{}", arg.name);
        match &arg.typ {
            spec_parser::protocol::Type::Int |
            spec_parser::protocol::Type::Uint |
            spec_parser::protocol::Type::Fixed |
            spec_parser::protocol::Type::Object |
            spec_parser::protocol::Type::NewId |
            spec_parser::protocol::Type::Fd |
            spec_parser::protocol::Type::Destructor => quote! {},
            spec_parser::protocol::Type::String => quote! {
                + ((self.#name.0.len() + 1 + 3) & !3) as u32 // + 1 for NUL byte
            },
            spec_parser::protocol::Type::Array => quote! {
                + ((self.#name.len() + 3) & !3) as u32
            },
        }
    });
    let serialize = request.args.iter().map(|arg| {
        let name = format_ident!("{}", arg.name);
        generate_serialize_for_type(iface_name, &name, arg, enum_info)
    });
    let deserialize = request.args.iter().map(|arg| {
        let name = format_ident!("{}", arg.name);
        let deserialize =
            generate_deserialize_for_type(iface_name, &arg.name, arg, enum_info, iface_version);
        quote! {
            #name: #deserialize
        }
    });
    let doc_comment = generate_doc_comment(&request.description);
    let nfds: u8 = request
        .args
        .iter()
        .filter(|arg| arg.typ == spec_parser::protocol::Type::Fd)
        .count()
        .try_into()
        .unwrap();
    let mut_ = if nfds != 0 {
        quote! {mut}
    } else {
        quote! {}
    };
    let extra_derives = if nfds != 0 {
        quote! {}
    } else {
        quote! {, Clone, Copy}
    };
    let public = quote! {
        #doc_comment
        #[derive(Debug, PartialEq, Eq #extra_derives)]
        pub struct #name #lifetime {
            #(#args),*
        }
        impl #lifetime #name #lifetime {
            pub const OPCODE: u16 = #opcode;
        }
        impl #lifetime ::runa_wayland_scanner::io::ser::Serialize for #name #lifetime {
            fn serialize<Fds: Extend<std::os::unix::io::OwnedFd>>(
                #mut_ self,
                buf: &mut ::runa_wayland_scanner::BytesMut,
                fds: &mut Fds,
            ) {
                use ::runa_wayland_scanner::BufMut;
                let msg_len = self.len() as u32;
                let prefix: u32 = (msg_len << 16) + (#opcode as u32);
                buf.put_u32_ne(prefix);
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
        impl<'a> ::runa_wayland_scanner::io::de::Deserialize<'a> for #name #lifetime {
            #[inline]
            fn deserialize(
                mut data: &'a [u8], mut fds: &'a [::std::os::unix::io::RawFd]
            ) -> Result<Self, ::runa_wayland_scanner::io::de::Error> {
                use ::runa_wayland_scanner::io::{pop_fd, pop_bytes, pop_i32, pop_u32};
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
    let hidden = if matches!(event_or_request, EventOrRequest::Event) {
        quote!(#[doc(hidden)])
    } else {
        quote!()
    };
    let methods = messages.iter().map(|m| {
        let name = if m.name == "move" || m.name == "type" {
            format_ident!("{}_", m.name)
        } else {
            format_ident!("{}", m.name)
        };
        let retty = format_ident!("{}Fut", m.name.to_pascal_case());
        let args = m.args.iter().map(|arg| {
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
        #hidden
        pub trait #ty<Ctx> {
            type Error;
            #(
                type #futs<'a>: ::std::future::Future<Output = ::std::result::Result<(), Self::Error>> + 'a
                    where Ctx: 'a;
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
        let public = messages.iter().enumerate().map(|(opcode, v)| {
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
                Self::#name(v) => v.serialize(buf, fds),
            }
        });
        let enum_deserialize_cases = messages.iter().enumerate().map(|(opcode, v)| {
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
            impl #enum_lifetime ::runa_wayland_scanner::io::ser::Serialize for #type_name #enum_lifetime {
                fn serialize<Fds: Extend<std::os::unix::io::OwnedFd>>(
                    self,
                    buf: &mut ::runa_wayland_scanner::BytesMut,
                    fds: &mut Fds,
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
            impl<'a> ::runa_wayland_scanner::io::de::Deserialize<'a> for #type_name #enum_lifetime {
                fn deserialize(
                    mut data: &'a [u8], mut fds: &'a [::std::os::unix::io::RawFd]
                ) -> ::std::result::Result<Self, ::runa_wayland_scanner::io::de::Error> {
                    use ::runa_wayland_scanner::io::pop_u32;
                    let _object_id = pop_u32(&mut data);
                    let header = pop_u32(&mut data);
                    let opcode = header & 0xFFFF;
                    match opcode {
                        #(#enum_deserialize_cases)*
                        _ => Err(
                            ::runa_wayland_scanner::io::de::Error::UnknownOpcode(
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
use regex::Regex;
lazy_static! {
    static ref LINKREF_REGEX: Regex = Regex::new(r"\[([0-9]+)\]").unwrap();
}
fn generate_doc_comment(description: &Option<(String, String)>) -> TokenStream {
    if let Some((summary, desc)) = description {
        let desc = desc.split('\n').map(|s| {
            let s = s.trim();
            if let Some(m) = LINKREF_REGEX.find(s) {
                if m.start() == 0 {
                    // Fix cases like "[0] link". Change it to "[0]: link"
                    let s: Cow<'_, _> = if !s[m.end()..].starts_with(':') {
                        format!("{}:{}", s[..m.end()].trim(), s[m.end()..].trim()).into()
                    } else {
                        s.into()
                    };
                    return quote! {
                        #[doc = #s]
                    }
                }
            }
            let s = wrap_links(s);
            quote! {
                #[doc = #s]
            }
        });
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
                ::runa_wayland_scanner::bitflags! {
                    #doc
                    #[derive(Copy, Clone, Hash, PartialEq, Eq, Debug)]
                    #[repr(transparent)]
                    pub struct #name: #repr {
                        #(#members)*
                    }
                }
            }
        } else {
            quote! {
                #doc
                #[derive(
                    ::runa_wayland_scanner::num_enum::IntoPrimitive,
                    ::runa_wayland_scanner::num_enum::TryFromPrimitive,
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
                    let is_int = arg.typ == spec_parser::protocol::Type::Int;

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
        #[allow(clippy::needless_lifetimes)]
        pub mod #name {
            use super::#name as __generated_root;
            #(#interfaces)*
        }
    })
}

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
fn enum_type_name(enum_: &str, iface_version: &HashMap<&str, u32>) -> Result<TokenStream> {
    Ok(if let Some((iface, name)) = enum_.split_once('.') {
        let version = iface_version
            .get(iface)
            .ok_or(Error::UnknownInterface(enum_.to_string()))?;
        let iface = format_ident!("{}", iface);
        let name = format_ident!("{}", name.to_pascal_case());
        let version = format_ident!("v{}", version);
        quote! {
            __generated_root::#iface::#version::enums::#name
        }
    } else {
        let name = format_ident!("{}", enum_.to_pascal_case());
        quote! {
            enums::#name
        }
    })
}
fn generate_arg_type_with_lifetime(
    arg: &wl_spec::protocol::Arg,
    lifetime: &Option<Lifetime>,
    iface_version: &HashMap<&str, u32>,
) -> Result<TokenStream> {
    use wl_spec::protocol::Type::*;
    if let Some(enum_) = &arg.enum_ {
        enum_type_name(enum_, iface_version)
    } else {
        Ok(match arg.typ {
            Int => quote!(i32),
            Uint => quote!(u32),
            Fixed => quote!(::wl_scanner_support::wayland_types::Fixed),
            Array =>
                if let Some(lifetime) = lifetime {
                    quote!(&#lifetime [u8])
                } else {
                    quote!(Box<[u8]>)
                },
            Fd => quote!(::wl_scanner_support::wayland_types::Fd),
            String =>
                if let Some(lifetime) = lifetime {
                    quote!(::wl_scanner_support::wayland_types::Str<#lifetime>)
                } else {
                    quote!(::wl_scanner_support::wayland_types::String)
                },
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

fn generate_serialize_for_type(
    current_iface_name: &str,
    name: &TokenStream,
    arg: &wl_spec::protocol::Arg,
    enum_info: &EnumInfos,
) -> Result<TokenStream> {
    use wl_spec::protocol::Type::*;
    if let Fd = arg.typ {
        return Ok(quote! {
            let pushed = writer
                .as_mut()
                .try_push_fds(&mut ::std::iter::once(#name.take().expect("trying to send raw fd")));
            assert_eq!(pushed, 1, "not enough space in writer");
        })
    }
    let get = match arg.typ {
        Int | Uint =>
            if let Some(enum_) = &arg.enum_ {
                let info = enum_info.get(current_iface_name, enum_)?;
                let repr = if info.is_int.unwrap_or(false) {
                    quote!(i32)
                } else {
                    quote!(u32)
                };
                if info.is_bitfield {
                    quote! {
                        let buf = #name.bits().to_ne_bytes();
                    }
                } else {
                    quote! {
                        let buf: #repr = #name.into();
                        let buf = buf.to_ne_bytes();
                    }
                }
            } else {
                quote! {
                    let buf = #name.to_ne_bytes();
                }
            },
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
    Ok(match arg.typ {
        Int | Uint | Fixed | Object | NewId => quote! {
            #get
            let written = writer.as_mut().try_write(&buf);
            assert_eq!(written, buf.len(), "not enough space in writer");
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
            let mut written = writer.as_mut().try_write(&len_buf);
            written += writer.as_mut().try_write(buf);
            written += writer.as_mut().try_write(&align_buf[..(aligned_len - buf.len() - 4)]);
            assert_eq!(written, aligned_len, "not enough space in writer");
        },
        Fd => unreachable!(),
        Destructor => quote! {},
    })
}
fn generate_deserialize_for_type(
    current_iface_name: &str,
    arg: &wl_spec::protocol::Arg,
    enum_info: &EnumInfos,
    iface_version: &HashMap<&str, u32>,
) -> Result<TokenStream> {
    use wl_spec::protocol::Type::*;
    Ok(match arg.typ {
        Int | Uint => {
            let v = if arg.typ == Int {
                quote! { let tmp = deserializer.pop_i32(); }
            } else {
                quote! { let tmp = deserializer.pop_u32(); }
            };
            let err = if arg.typ == Int {
                quote! { ::wl_scanner_support::io::de::Error::InvalidIntEnum }
            } else {
                quote! { ::wl_scanner_support::io::de::Error::InvalidUintEnum }
            };
            if let Some(enum_) = &arg.enum_ {
                let is_bitfield = enum_info.get(current_iface_name, enum_)?.is_bitfield;
                let enum_ty = enum_type_name(enum_, iface_version)?;
                let enum_ty = quote! { super::#enum_ty };
                if is_bitfield {
                    quote! { {
                        #v
                        #enum_ty::from_bits(tmp).ok_or_else(|| #err(tmp, std::any::type_name::<#enum_ty>()))?
                    } }
                } else {
                    quote! { {
                        #v
                        #enum_ty::try_from(tmp).map_err(|_| #err(tmp, std::any::type_name::<#enum_ty>()))?
                    } }
                }
            } else {
                quote! { { #v tmp } }
            }
        },
        Fixed => quote! { deserializer.pop_i32().into() },
        Object | NewId => quote! { deserializer.pop_u32().into() },
        Fd => quote! { deserializer.pop_fd().into() },
        String | Array => quote! { {
            let len = deserializer.pop_u32();
            deserializer.pop_bytes(len as usize).into()
        } },
        Destructor => quote! {},
    })
}
fn generate_message_variant(
    iface_name: &str,
    opcode: u16,
    request: &Message,
    is_owned: bool,
    _parent: &Ident,
    iface_version: &HashMap<&str, u32>,
    enum_info: &EnumInfos,
    event_or_request: EventOrRequest,
    parent_is_borrowed: bool,
) -> Result<(TokenStream, TokenStream)> {
    let args = &request.args;
    let args: Result<TokenStream> = args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            let ty = generate_arg_type(&arg, is_owned, iface_version)?;
            let doc_comment = generate_doc_comment(&arg.description);
            Ok(quote! {
                #doc_comment
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
    let parent_type_name = match event_or_request {
        EventOrRequest::Event => format_ident!("Event"),
        EventOrRequest::Request => format_ident!("Request"),
    };
    let parent_lifetime = if parent_is_borrowed {
        quote!(<'a>)
    } else {
        quote!()
    };
    let lifetime = if is_borrowed { quote!(<'a>) } else { quote!() };
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
        .count() as u32 *
        4;
    let variable_len: TokenStream = request
        .args
        .iter()
        .map(|arg| {
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
        })
        .collect();
    let serialize = request
        .args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            let name = quote! { self.#name };
            let serialize = generate_serialize_for_type(iface_name, &name, &arg, enum_info)?;
            Ok(quote! {
                #serialize
            })
        })
        .collect::<Result<TokenStream>>()?;
    let deserialize = request
        .args
        .iter()
        .map(|arg| {
            let name = format_ident!("{}", arg.name);
            let deserialize =
                generate_deserialize_for_type(iface_name, &arg, enum_info, iface_version)?;
            Ok(quote! {
                #name: #deserialize,
            })
        })
        .collect::<Result<TokenStream>>()?;
    let deserializer_var = if request.args.is_empty() {
        quote! { _deserializer }
    } else {
        quote! { mut deserializer }
    };
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
            #args
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
                let written = writer.as_mut().try_write(&prefix.to_ne_bytes());
                assert_eq!(written, 4, "not enough space in writer");
                #serialize
            }
            #[inline]
            fn len(&self) -> u16 {
                (#fixed_len #variable_len + 8) as u16 // 8 is the header
            }
            #[inline]
            fn nfds(&self) -> u8 {
                #nfds
            }
        }
        impl<'a> ::wl_scanner_support::io::de::Deserialize<'a> for #name #lifetime {
            #[inline]
            fn deserialize<D: ::wl_scanner_support::io::de::Deserializer<'a>>(
                #deserializer_var: D
            ) -> Result<Self, ::wl_scanner_support::io::de::Error> {
                Ok(Self {
                    #deserialize
                })
            }
        }
        impl #parent_lifetime Into<super::#parent_type_name #parent_lifetime> for #name #lifetime {
            #[inline]
            fn into(self) -> super::#parent_type_name #parent_lifetime {
                super::#parent_type_name::#name(self)
            }
        }
    };

    Ok((public, quote! {}))
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
            let retty = format_ident!("{}Fut", m.name.to_pascal_case());
            let args = m
                .args
                .iter()
                .map(|arg| {
                    let name = format_ident!("{}", arg.name);
                    let typ = generate_arg_type(&arg, false, iface_version)?;
                    Ok(quote! {
                        #name: #typ
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let doc_comment = generate_doc_comment(&m.description);
            Ok(quote! {
                #doc_comment
                fn #name<'a>(
                    &'a self,
                    ctx: &'a mut Ctx,
                    object_id: u32,
                    #(#args),*
                )
                -> Self::#retty<'a>;
            })
        })
        .collect::<Result<TokenStream>>()?;
    let futs = messages
        .iter()
        .map(|m| format_ident!("{}Fut", m.name.to_pascal_case()));
    Ok(quote! {
        pub trait #ty<Ctx> {
            type Error: ::wl_scanner_support::ProtocolError;
            #(
                type #futs<'a>: ::std::future::Future<Output = ::std::result::Result<(), Self::Error>> + 'a
                    where Ctx: 'a, Self: 'a;
            )*
            #methods
        }
    })
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
        let enum_is_borrowed = messages
            .iter()
            .any(|v| v.args.iter().any(|arg| type_is_borrowed(&arg.typ)));
        let (public, private): (TokenStream, TokenStream) = messages
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
                    event_or_request,
                    enum_is_borrowed,
                )
            })
            .collect::<Result<Vec<(TokenStream, TokenStream)>>>()?
            .into_iter()
            .unzip();
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
        let enum_deserialize_cases: TokenStream = messages
            .iter()
            .enumerate()
            .map(|(opcode, v)| {
                let name = format_ident!("{}", v.name.to_pascal_case());
                let opcode = opcode as u32;
                quote! {
                    #opcode => Ok(Self::#name(<#mod_name::#name>::deserialize(deserializer)?)),
                }
            })
            .collect();
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
        let dispatch = generate_dispatch_trait(messages, event_or_request, iface_version)?;
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
                fn deserialize<D: ::wl_scanner_support::io::de::Deserializer<'a>>(
                    mut deserializer: D
                ) -> ::std::result::Result<Self, ::wl_scanner_support::io::de::Error> {
                    let _object_id = deserializer.pop_u32();
                    let header = deserializer.pop_u32();
                    let opcode = header & 0xFFFF;
                    match opcode {
                        #enum_deserialize_cases
                        _ => Err(
                            ::wl_scanner_support::io::de::Error::UnknownOpcode(
                                opcode, std::any::type_name::<Self>())),
                    }
                }
            }
        };
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
            } else {
                if is_bitfield {
                    format_ident!("{}", e.name.TO_SHOUTY_SNEK_CASE())
                } else {
                    format_ident!("{}", e.name.to_pascal_case())
                }
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
    let enums = generate_enums(&iface.enums, &iface.name, enum_info);

    let iface_name = &iface.name;
    let iface_version = iface.version;
    Ok(quote! {
        #doc_comment
        pub mod #name {
            pub mod #version {
                #[allow(unused_imports)]
                use super::super::__generated_root;
                /// Name of the interface
                pub const NAME: &str = #iface_name;
                pub const VERSION: u32 = #iface_version;
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

fn scan_enum(proto: &Protocol, enum_info: &mut EnumInfos) -> Result<()> {
    for iface in proto.interfaces.iter() {
        for req in iface.requests.iter().chain(iface.events.iter()) {
            for arg in req.args.iter() {
                if let Some(ref enum_) = arg.enum_ {
                    let info = enum_info.get_mut(&iface.name, &enum_)?;
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

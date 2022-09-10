use heck::ToPascalCase;
use proc_macro2::{Ident, TokenStream};
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
        Fd => quote!(::wl_scanner_support::wayland_types::Fd),
        String => {
            if is_owned {
                quote!(::wl_scanner_support::wayland_types::String)
            } else {
                quote!(::wl_scanner_support::wayland_types::Str<'a>)
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

pub fn generate_deserialize_accessor(ty: &Ident, is_borrowed: bool) -> TokenStream {
    let accessor_name = format_ident!("{}Accessor", ty);
    let accessor_parameter = if is_borrowed {
        quote!(<'a, R>)
    } else {
        quote!(<R>)
    };
    let accessor_parameter_no_bound = if is_borrowed {
        quote!(<'a, R>)
    } else {
        quote!(<R>)
    };
    let ty_lifetime = if is_borrowed { quote!(<'a>) } else { quote!() };
    quote! {
    /// Holds a value that is deserialized from borrowed data from `reader`.
    /// This prevents the reader from being used again while this accessor is alive.
    /// Also this accessor will advance the reader to the next message when dropped.
    pub struct #accessor_name #accessor_parameter
    where
        R: ::wl_scanner_support::io::AsyncBufRead,
    {
        result: #ty #ty_lifetime,
        reader: ::std::ptr::NonNull<R>,
        len: usize,
    }

    impl #accessor_parameter #accessor_name #accessor_parameter_no_bound
    where
        R: ::wl_scanner_support::io::AsyncBufRead,
    {
        // Safety: result must be borrowing from `reader`, `reader` must be valid. (i.e. reader
        // must outlive result). _And_ `reader` must have already been pinned
        pub(super) unsafe fn new(reader: ::std::ptr::NonNull<R>, result: #ty #ty_lifetime, len: usize) -> Self {
            Self {
                result,
                reader,
                len,
            }
        }
    }
    impl #accessor_parameter ::std::fmt::Debug for #accessor_name #accessor_parameter_no_bound
    where
        R: ::wl_scanner_support::io::AsyncBufRead,
    {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
            self.result.fmt(f)
        }
    }
    impl #accessor_parameter Drop for #accessor_name #accessor_parameter_no_bound
    where
        R: ::wl_scanner_support::io::AsyncBufRead,
    {
        fn drop(&mut self) {
            use ::wl_scanner_support::io::AsyncBufReadExt;
            // Safety: based on contrat of `new`, `reader` must outlive `result`, which we borrow.
            // And `reader` must be pinned already.
            unsafe { ::std::pin::Pin::new_unchecked(self.reader.as_mut()).consume(self.len); }
        }
    }
    impl #accessor_parameter ::std::ops::Deref for #accessor_name #accessor_parameter_no_bound
    where
        R: ::wl_scanner_support::io::AsyncBufRead,
    {
        type Target = #ty #ty_lifetime;
        fn deref(&self) -> &Self::Target {
            &self.result
        }
    }
    impl #accessor_parameter ::std::ops::DerefMut for #accessor_name #accessor_parameter_no_bound
    where
        R: ::wl_scanner_support::io::AsyncBufRead,
    {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.result
        }
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
            use ::std::os::unix::io::AsRawFd;
            let fd = #name.as_raw_fd();
            ::wl_scanner_support::ready!(self.writer.as_mut().poll_write_with_fds(cx, &[], &[fd]))?;
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
            let buf = #name.0.to_bytes_with_nul();
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
        #[derive(::wl_scanner_support::serde::Deserialize, Debug)]
        #[serde(crate = "::wl_scanner_support::serde")]
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventOrRequest {
    Event,
    Request,
}

pub fn generate_event_or_request(
    _name: &str,
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
        let accessor = generate_deserialize_accessor(&type_name, enum_is_borrowed);
        let accessor_name = format_ident!("{}Accessor", type_name);
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
        let accessor_parameter = if enum_is_borrowed {
            quote! { <'a, D> }
        } else {
            quote! { <D> }
        };
        let deserialize = messages
            .iter()
            .enumerate()
            .map(|(opcode, v)| {
                let name = format_ident!("{}", v.name.to_pascal_case());
                let opcode = opcode as u32;
                quote! {
                    #opcode => {
                        #type_name::#name(
                            ::wl_scanner_support::serde::de::Deserialize::deserialize(&mut body)?
                        )
                    }
                }
            })
            .collect::<TokenStream>();
        let public = quote! {
            #accessor
            pub mod #mod_name { #public }
            #[doc = "Collection of all possible types of messages, see individual message types "]
            #[doc = "for more information."]
            #[derive(Debug)]
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

            /// Deserialize a message. This is implemented on the accessor to make sure user
            /// doesn't forget to advance the buffer in the reader.
            ///
            /// This is also not implemented for each of the message variants because to
            /// efficiently deserialize a message we need to know the message length beforehand,
            /// however message header is not part of the individual variants.
            impl<'a, 'de: 'a, D> ::wl_scanner_support::io::Deserialize<'de, D> for
            #accessor_name #accessor_parameter
            where
                D: ::wl_scanner_support::io::AsyncBufReadExt +
                   ::wl_scanner_support::io::AsyncBufRead +
                   ::wl_scanner_support::io::AsyncReadWithFds +
                   'de
            {
                type Error = ::wl_scanner_support::Error;
                fn poll_deserialize(mut reader: ::std::pin::Pin<&'de mut D>, cx: &mut ::std::task::Context<'_>)
                -> ::std::task::Poll<::std::result::Result<Self, Self::Error>> {
                    use ::wl_scanner_support::io::{AsyncBufReadExt as _, de::WaylandDeserializer};
                    use ::wl_scanner_support::Error;
                    use ::std::io::{Error as StdError, ErrorKind};
                    use ::wl_scanner_support::ready;
                    use ::std::task::Poll;
                    use ::std::pin::Pin;
                    use ::std::ptr::NonNull;
                    let buf = ready!(reader.as_mut().poll_fill_buf_until(cx, 4))?;
                    let header = buf.get(..4)
                                    .ok_or_else(|| StdError::new(ErrorKind::UnexpectedEof, "Unexpected EOF"))?;
                    // Safety: we just checked that the buffer is exactly 4 bytes long
                    let header = u32::from_ne_bytes(unsafe { *(header.as_ptr() as *const [u8; 4]) });
                    let len = (header >> 16) - 4; // minus 4 byte object ID we assume is already
                                                  // read
                    let opcode = header & 0xffff;
                    // Make sure data is ready, we don't hold the buffer yet, because if we do,
                    // we won't be able to get file descriptors from the reader anymore. Filling
                    // the buffer to `len` bytes should have made the file descriptors available to
                    // us as well, as per unix(7).
                    let _ = ready!(reader.as_mut().poll_fill_buf_until(cx, len as usize))?;
                    // Safety: we don't move the reader using the &mut from get_unchecked_mut, we
                    // just case it to a *mut
                    let (raw_reader, buf) = unsafe {
                        let reader = reader.get_unchecked_mut();
                        let mut raw_reader: NonNull<D> = reader.into();
                        // Safety:
                        // 1. raw_reader was pinned and we didn't move it.
                        // 2. poll_fill_buf_until must originate from raw_reader to not violate
                        //    stacked borrow rules.
                        // 3. buf is give a 'de lifetime so it borrows reader (probably
                        //    unnecessary)
                        let buf: &'de [u8] = ready!(Pin::new_unchecked(raw_reader.as_mut())
                            .poll_fill_buf_until(cx, len as usize))?;
                        (raw_reader, buf)
                    };
                    let body = buf.get(4..len as usize)
                                  .ok_or_else(|| StdError::new(ErrorKind::UnexpectedEof, "Unexpected EOF"))?;
                    let mut body = WaylandDeserializer::new(body);
                    let result = match opcode {
                        #deserialize
                        _ => return Poll::Ready(Err(Self::Error::UnknownOpcode(opcode))),
                    };
                    // Safety: result _does_ borrow from raw_reader
                    Poll::Ready(Ok(unsafe {
                        Self::new(
                            raw_reader,
                            result,
                            len as usize,
                        )
                    }))
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
pub fn generate_doc_comment(description: &Option<(String, String)>) -> TokenStream {
    if let Some((summary, desc)) = description {
        let desc = desc
            .split("\n")
            .map(|s| {
                let s = s.trim();
                quote! {
                    #[doc = #s]
                }
            })
            .collect::<TokenStream>();
        quote! {
            #[doc = #summary]
            #[doc = ""]
            #desc
        }
    } else {
        quote! {}
    }
}
pub fn generate_interface(iface: &Interface) -> Result<TokenStream> {
    let name = format_ident!("{}", iface.name);
    let version = format_ident!("v{}", iface.version);

    let (requests, requests_private) =
        generate_event_or_request(&iface.name, &iface.requests, EventOrRequest::Request)?;
    let (events, events_private) =
        generate_event_or_request(&iface.name, &iface.events, EventOrRequest::Event)?;
    let doc_comment = generate_doc_comment(&iface.description);

    Ok(quote! {
        #doc_comment
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

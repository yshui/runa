extern crate proc_macro;
use darling::{FromDeriveInput, FromMeta, FromVariant};
use proc_macro_error::ResultExt;
use syn::parse::Parse;

macro_rules! die {
    ($spanned:expr=>
        $msg:expr
    ) => {
        return Err(Error::new_spanned($spanned, $msg))
    };

    (
        $msg:expr
    ) => {
        return Err(Error::new(Span::call_site(), $msg))
    };
}

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

struct Reject;

impl darling::FromField for Reject {
    fn from_field(_field: &syn::Field) -> Result<Self, darling::Error> {
        Err(darling::Error::unsupported_shape(
            "fields are not supported",
        ))
    }
}

/// Generate implementation of `MessageBroker` for a give collection of
/// interfaces. Each interface in this enum should have a `#[wayland(impl =
/// ...)]` attribute, pointing to the implementation type of the interface. Each
/// of the implementation types must implement the `InterfaceMessageDispatch`
/// trait, with `Ctx = connection_context`. And all of these implementations
/// must share the same Error type. This Error type must accept conversion from
/// `std::io::Error`.
///
/// (The `InterfaceMessageDispatch` can be generated using the
/// `interface_message_dispatch` macro.)
///
/// Your crate must depends the `wl_protocol` and the `wl_common` crate to use
/// this. (TODO: allow override)
///
/// # Field arguments
///
/// * `impl`: The implementation type of the interface. It must implement the
///   <interface>::Dispatch trait.
///
/// # Enum arguments
///
/// * `connection_context`: The context type that is passed to `dispatch`
///   functions. All the type argument `Ctx` in the field arguments will be
///   replaced with this type.
///
/// # Example
///
/// ```rust
/// #[message_broker]
/// #[wayland(connection_context = "crate::MyServer")]
/// enum Interfaces {
///     #[wayland(
///         impl = "wl_compositor::WlCompositor",
///         version = 1,
///         protocol = "wayland"
///     )]
///     WlCompositor,
/// }
/// ```
#[proc_macro_attribute]
pub fn message_broker(
    _attrs: proc_macro::TokenStream,
    tokens: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    use quote::quote;
    use syn::Error;

    #[derive(FromVariant)]
    #[darling(attributes(wayland))]
    struct Interface {
        #[darling(rename = "impl")]
        imp:    syn::TypePath,
        ident:  syn::Ident,
        #[allow(unused)]
        fields: darling::ast::Fields<Reject>,
    }

    #[derive(FromDeriveInput)]
    #[darling(attributes(wayland))]
    struct Interfaces {
        ident:              syn::Ident,
        data:               darling::ast::Data<Interface, Reject>,
        connection_context: syn::Type,
    }
    impl syn::parse::Parse for Interfaces {
        fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
            let input: syn::DeriveInput = input.parse()?;
            if input.generics.params.len() != 0 {
                die!(input.generics=>
                    "Types with generic parameters are not supported"
                );
            }
            let input2 = Interfaces::from_derive_input(&input)?;
            if input2.data.is_struct() {
                die!(input =>
                    "Must be an enum"
                );
            }
            let enum_ = input2.data.as_ref().take_enum().unwrap();
            if enum_.iter().any(|v| !v.fields.is_unit()) {
                die!(input =>
                    "All variants must be unit variants"
                );
            }
            Ok(input2)
        }
    }
    let input = syn::parse_macro_input!(tokens as Interfaces);
    let mut enum_ = input.data.take_enum().unwrap();
    // Replace the Ctx type argument with the server context types
    for iface in &mut enum_ {
        let path = &mut iface.imp.path;
        for seg in path.segments.iter_mut() {
            if let syn::PathArguments::AngleBracketed(ref mut args) = seg.arguments {
                for arg in args.args.iter_mut() {
                    match arg {
                        syn::GenericArgument::Type(ty) =>
                            if let syn::Type::Path(path) = ty {
                                if path.path.is_ident("Ctx") {
                                    *ty = input.connection_context.clone();
                                }
                            },
                        syn::GenericArgument::Binding(binding) => {
                            if let syn::Type::Path(path) = &mut binding.ty {
                                if path.path.is_ident("Ctx") && path.qself.is_none() {
                                    binding.ty = input.connection_context.clone();
                                }
                            }
                        },
                        _ => (),
                    }
                }
            }
        }
    }
    let name = &input.ident;
    let ret = enum_.iter().map(|i| {
        use heck::ToSnakeCase;
        let iface = i.ident.to_string().to_snake_case();
        let imp = &i.imp;
        quote! {
            #iface => {
                use ::wl_server::provide_any::request_ref;
                use ::std::ops::Deref;
                let real_obj: &#imp = request_ref(obj.deref())
                    .expect("Wrong InterfaceMeta impl");
                match ::wl_common::InterfaceMessageDispatch::dispatch(real_obj, ctx, object_id, &mut de).await {
                    Ok(()) => false,
                    Err(e) => {
                        if let Some((object_id, error_code)) = e.wayland_error() {
                            if ctx.send(DISPLAY_ID, wl_display::events::Error {
                                code: error_code,
                                object_id: wl_types::Object(object_id),
                                message: wl_types::String::from(e.to_string()).as_str(),
                            }).await.is_err() {
                                return true
                            }
                        }
                        e.fatal()
                    }
                }
            }
        }
    });
    let ctx = &input.connection_context;
    let inits = enum_.iter().map(|i| {
        let imp = &i.imp;
        quote! { #imp::init_server(&mut builder).unwrap(); }
    });
    let handle_events = enum_.iter().map(|i| {
        let imp = &i.imp;
        quote! { #imp::handle_events(ctx, i, slot_names[i]).await?; }
    });

    // Get the error type of the first interface. They are all supposed to be the
    // same.
    let imp0 = &enum_[0].imp;
    let error = quote! {
        <#imp0 as ::wl_common::InterfaceMessageDispatch<#ctx>>::Error
    };
    let orig_var = enum_.iter().map(|i| &i.ident);
    quote! {
        pub enum #name {
            #(#orig_var),*
        }
        const _: () = {
            use std::pin::Pin;
            impl #name {
                fn init_server() -> <#ctx as ::wl_server::connection::Connection>::Context {
                    let mut builder = <<#ctx as ::wl_server::connection::Connection>::Context as ::wl_server::server::Server>::builder();
                    #(#inits)*
                    use ::wl_server::server::ServerBuilder;
                    builder.build()
                }
                /// Dispatch a message from `reader` to the context. Returns whether the client
                /// needs to be disconnected.
                async fn dispatch<'a, 'b: 'a, R>(
                    ctx: &'a mut #ctx,
                    mut reader: Pin<&mut R>
                ) -> bool
                where
                    R: ::wl_common::__private::AsyncBufReadWithFd + 'b,
                {
                    use ::wl_server::{__private::{wl_types, wl_display::v1 as wl_display}, objects::DISPLAY_ID};
                    use ::wl_protocol::ProtocolError;
                    let (object_id, len, mut de) = match ::wl_common::Deserializer::next_message(reader.as_mut()).await {
                        Ok(v) => v,
                        Err(e) => return true,
                    };
                    let obj = ctx.objects().get(object_id);
                    let ret = match &obj {
                        Some(ref obj) => {
                            match obj.interface() {
                                #(#ret),*,
                                _ => unreachable!(),
                            }
                        },
                        None => {
                            // We are going to disconnect the client so we don't care about the
                            // error.
                            let _: Result<_, _> = ctx.send(DISPLAY_ID,
                                wl_display::events::Error {
                                    code: wl_display::enums::Error::InvalidObject as u32,
                                    object_id: wl_types::Object(object_id),
                                    message: wl_types::str!("Invalid object id"),
                                }
                            ).await;
                            true
                        }
                    };
                    // TODO: check if there is leftover data and fail if so
                    reader.consume(len);
                    ret
                }
                async fn handle_events(ctx: &mut #ctx) -> Result<(), #error> {
                    use ::wl_server::connection::Evented;
                    use ::wl_server::server::{EventSource, Server};
                    let events = ctx.reset_events();
                    let slot_names = ctx.server_context().slots();
                    for i in events.iter_ones() {
                        #(#handle_events)*
                    }
                    Ok(())
                }
            }
        };
    }
    .into()
}

struct DispatchItem {
    ident: syn::Ident,
    args:  Vec<syn::Ident>,
}

struct DispatchImpl {
    generics: syn::Generics,
    trait_:   syn::Path,
    self_ty:  syn::Type,
    items:    Vec<DispatchItem>,
    error:    syn::Type,
}

impl Parse for DispatchImpl {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        use syn::Error;
        let input: syn::ItemImpl = input.parse()?;
        let (_, trait_, _) = input
            .trait_
            .as_ref()
            .ok_or_else(|| Error::new_spanned(&input, "Must be an impl of a trait"))?
            .clone();
        let last_seg = trait_
            .segments
            .last()
            .ok_or_else(|| Error::new_spanned(&input, "Trait path must not be empty"))?;
        match last_seg.arguments {
            syn::PathArguments::None | syn::PathArguments::Parenthesized(_) => {
                die!(trait_ =>
                    "Trait must have exactly one type parameter (the Ctx type)"
                );
            },
            syn::PathArguments::AngleBracketed(ref args) =>
                if args.args.len() != 1 {
                    die!(trait_ =>
                        "Trait must have exactly one type parameter (the Ctx type)"
                    );
                },
        }

        let mut error = None;
        let mut items = Vec::new();
        for item in &input.items {
            match item {
                syn::ImplItem::Method(method) => {
                    let mut args = Vec::new();
                    for arg in method.sig.inputs.iter().skip(3) {
                        // Ignore the first 3 arguments: self, ctx, object_id
                        match arg {
                            syn::FnArg::Receiver(_) => (),
                            syn::FnArg::Typed(patty) =>
                                if let syn::Pat::Ident(pat) = &*patty.pat {
                                    args.push(pat.ident.clone());
                                } else {
                                    die!(&patty.pat =>
                                        "Argument must be a simple identifier"
                                    );
                                },
                        }
                    }
                    items.push(DispatchItem {
                        ident: method.sig.ident.clone(),
                        args,
                    });
                },
                syn::ImplItem::Type(ty) =>
                    if ty.ident == "Error" {
                        if error.is_none() {
                            error = Some(ty.ty.clone());
                        } else {
                            die!(ty=>
                                "Only one Error type is allowed"
                            );
                        }
                    } else if !ty.ident.to_string().ends_with("Fut") {
                        die!(ty=>
                            "Only Error and *Fut type items are allowed"
                        );
                    },
                _ => die!(item=>
                    "Unrecognized item"
                ),
            }
        }
        let error = error.ok_or_else(|| Error::new_spanned(&input, "No Error type found"))?;
        Ok(DispatchImpl {
            generics: input.generics,
            trait_,
            self_ty: *input.self_ty,
            items,
            error,
        })
    }
}

macro_rules! unwrap {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return e.to_compile_error().into(),
        }
    };
}

/// Convert path arguments in a path to turbofish style.
fn as_turbofish(path: &syn::Path) -> syn::Path {
    let mut path = path.clone();
    path.segments
        .iter_mut()
        .for_each(|seg| match &mut seg.arguments {
            syn::PathArguments::AngleBracketed(ref mut args) => {
                args.colon2_token = Some(Default::default());
            },
            _ => (),
        });
    path
}

/// Generate `wl_common::InterfaceMessageDispatch` for types that implement
/// `RequestDispatch` for a certain interface. The should be put on top of the
/// `RequestDispatch` impl. Your impl of `RequestDispatch` should contains an
/// error type that can be converted from serde deserailization error.
///
/// You need to import the `wl_common` crate to use this macro.
///
/// # Arguments
///
/// * `message` - The message type. By default, this attribute try to cut the
///   "Dispatch" suffix from the trait name. i.e.
///   `wl_buffer::v1::RequestDispatch` will become `wl_buffer::v1::Request`.
#[proc_macro_attribute]
pub fn interface_message_dispatch(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    use heck::ToPascalCase;
    use quote::{format_ident, quote};
    #[derive(FromMeta)]
    struct Attributes {
        #[darling(default)]
        message: Option<syn::Path>,
    }
    let orig_item = item.clone();
    let orig_item = syn::parse_macro_input!(orig_item as syn::ItemImpl);
    let item: DispatchImpl = syn::parse_macro_input!(item);
    let attr: syn::AttributeArgs = syn::parse_macro_input!(attr);
    let attr = Attributes::from_list(&attr).unwrap();

    let message_ty = unwrap!(attr.message.map_or_else(
        || {
            let mut trait_ = item.trait_.clone();
            let last_seg = trait_.segments.last_mut().ok_or_else(|| {
                syn::Error::new_spanned(&item.trait_, "Trait path must not be empty")
            })?;

            let last_ident = last_seg.ident.to_string();
            if last_ident.ends_with("Dispatch") {
                last_seg.ident = syn::parse_str(last_ident.trim_end_matches("Dispatch"))?;
                last_seg.arguments = syn::PathArguments::None;
                Ok(trait_)
            } else {
                Err(syn::Error::new_spanned(
                    &item.trait_,
                    "Trait name does not end with Dispatch, and no message type is specified",
                ))
            }
        },
        Result::Ok
    ));

    // Generate InterfaceMessageDispatch impl. We just need to replace the trait
    // name with InterfaceMessageDispatch.
    let DispatchImpl {
        generics,
        trait_,
        self_ty,
        items,
        error,
    } = item;

    let mut last_seg = unwrap!(trait_
        .segments
        .last()
        .ok_or_else(|| { syn::Error::new_spanned(&trait_, "Trait path must not be empty") }))
    .clone();
    last_seg.ident = format_ident!("InterfaceMessageDispatch");
    match last_seg.arguments {
        syn::PathArguments::Parenthesized(_) | syn::PathArguments::None =>
            return syn::Error::new_spanned(&last_seg, "Trait must have a single type parameter")
                .to_compile_error()
                .into(),
        syn::PathArguments::AngleBracketed(ref args) => {
            if args.args.len() != 1 {
                return syn::Error::new_spanned(&last_seg, "Trait must have a single type parameter")
                    .to_compile_error()
                    .into()
            }
            match args.args[0] {
                syn::GenericArgument::Type(syn::Type::Path(ref path)) => path.path.clone(),
                _ =>
                    return syn::Error::new_spanned(
                        &last_seg,
                        "Trait must have a single type parameter",
                    )
                    .to_compile_error()
                    .into(),
            }
        },
    };
    let our_trait = syn::Path {
        leading_colon: Some(syn::token::Colon2::default()),
        segments:      [
            syn::PathSegment {
                ident:     format_ident!("wl_common"),
                arguments: syn::PathArguments::None,
            },
            last_seg,
        ]
        .into_iter()
        .collect(),
    };
    let where_clause = generics.where_clause.as_ref();

    let match_items = items.iter().map(|item| {
        let var = format_ident!("{}", item.ident.to_string().to_pascal_case());
        let trait_ = as_turbofish(&trait_);
        let ident = &item.ident;
        let args = item.args.iter().map(|arg| {
            if arg.to_string().starts_with("_") {
                format_ident!("{}", arg.to_string().trim_start_matches("_"))
            } else {
                arg.clone()
            }
        });
        quote! {
            #message_ty::#var(msg) => {
                #trait_::#ident(self, ctx, object_id, #(msg.#args),*).await
            }
        }
    });

    //let combined_future_var = items.iter().map(|item| {
    //    format_ident!("{}Fut", item.ident.to_string().to_pascal_case())
    //});
    //let combined_future_var2 = combined_future_var.clone();

    quote! {
        #orig_item
        const _: () = {
            // TODO: generate this is combined future is too complicated, especially because the
            //       ctx parameter: it can be concrete, it can have where clauses, etc. so we patch
            //       things together with TAIT for now. hopefully that will be stabilized and we
            //       don't ever need to look at this again.
            //pub enum CombinedFut<'a, #ctx_param> {
            //    #(#combined_future_var(<#self_ty as #trait_>::#combined_future_var<'a>)),*
            //}
            //impl<'a> ::std::future::Future for CombinedFut<'a> {
            //    type Output = Result<(), #error>;
            //    fn poll(
            //        self: ::std::pin::Pin<&mut Self>,
            //        cx: &mut ::std::task::Context<'_>
            //    ) -> ::std::task::Poll<Self::Output> {
            //        // We use some unsafe code to get Pin<&mut> of inner futures.
            //        // Safety: self is pinned, so the variants are pinned to, and we are not going
            //        // to move them with the &mut we get here.
            //        unsafe {
            //            match self.get_unchecked_mut() {
            //                #(Self::#combined_future_var2(ref mut fut) => {
            //                    ::std::pin::Pin::new_unchecked(fut).poll(cx)
            //                }),*
            //            }
            //        }
            //    }
            //}
            impl #generics #our_trait for #self_ty #where_clause {
                type Error = #error;
                type Fut<'a, R> = impl ::std::future::Future<Output = ::std::result::Result<(), Self::Error>> + 'a
                where
                    Self: 'a,
                    Ctx: 'a,
                    R: 'a + ::wl_common::__private::AsyncBufReadWithFd;
                fn dispatch<'a, R>(
                    &'a self,
                    ctx: &'a mut Ctx,
                    object_id: u32,
                    reader: &mut ::wl_common::Deserializer<'a, R>,
                ) -> Self::Fut<'a, R>
                where
                    R: ::wl_common::__private::AsyncBufReadWithFd
                {
                    let msg: ::std::result::Result<#message_ty, _> = reader.deserialize();
                    // TODO: check if the length of the message matches the expected length
                    async move {
                        // TODO: handle deserialization error, send the client an error message
                        let msg = msg?;
                        match msg {
                            #(#match_items),*
                        }
                    }
                }
            }
        };
    }
    .into()
}

extern crate proc_macro;

use darling::{FromDeriveInput, FromMeta};
use proc_macro_error::ResultExt;
use quote::{quote, ToTokens};
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
    let path =
        std::path::Path::new(&std::env::var("CARGO_WORKSPACE_DIR").unwrap()).join(path.value());
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
    path.segments.iter_mut().for_each(|seg| {
        if let syn::PathArguments::AngleBracketed(ref mut args) = &mut seg.arguments {
            args.colon2_token = Some(Default::default());
        }
    });
    path
}

/// Generate `wl_common::InterfaceMessageDispatch` for types that implement
/// `RequestDispatch` for a certain interface.
///
/// It deserialize a message from a deserializer, and calls appropriate function
/// in the `RequestDispatch` based on the message content. Your impl of
/// `RequestDispatch` should contains an error type that can be converted from
/// deserailization error.
///
/// # Arguments
///
/// * `message` - The message type. By default, this attribute try to cut the
///   "Dispatch" suffix from the trait name. i.e.
///   `wl_buffer::v1::RequestDispatch` will become `wl_buffer::v1::Request`.
///   Only valid as a impl block attribute.
/// * `crate` - The path to the `wl_server` crate. "wl_server" by default.
#[proc_macro_attribute]
pub fn interface_message_dispatch(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    use heck::ToPascalCase;
    use quote::format_ident;
    #[derive(FromMeta)]
    struct Attributes {
        #[darling(default)]
        message: Option<syn::Path>,
        #[darling(default, rename = "crate")]
        crate_:  Option<syn::LitStr>,
    }
    let stream = item.clone();
    let orig_item = syn::parse_macro_input!(item as syn::ItemImpl);
    let item: DispatchImpl = syn::parse_macro_input!(stream);
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
    let crate_: syn::Path = attr.crate_.map_or_else(
        || syn::parse_str("::wl_server").unwrap(),
        |s| syn::parse_str(&s.value()).unwrap(),
    );

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
    let our_trait: syn::Path = syn::parse_quote! { #crate_::__private::#last_seg };
    let where_clause = generics.where_clause.as_ref();

    let match_items = items.iter().map(|item| {
        let var = format_ident!("{}", item.ident.to_string().to_pascal_case());
        let trait_ = as_turbofish(&trait_);
        let ident = &item.ident;
        let args = item.args.iter().map(|arg| {
            if arg.to_string().starts_with('_') {
                format_ident!("{}", arg.to_string().trim_start_matches('_'))
            } else {
                arg.clone()
            }
        });
        quote! {
            #message_ty::#var(msg) => {
                (#trait_::#ident(self, ctx, object_id, #(msg.#args),*).await, bytes_read, fds_read)
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
                type Fut<'a> = impl ::std::future::Future<Output = (::std::result::Result<(), Self::Error>, usize, usize)> + 'a
                where
                    Self: 'a,
                    Ctx: 'a;
                fn dispatch<'a>(
                    &'a self,
                    ctx: &'a mut Ctx,
                    object_id: u32,
                    reader: (&'a [u8], &'a [::std::os::unix::io::RawFd]),
                ) -> Self::Fut<'a>
                {
                    let res: Result<(_, _, _), _> =
                        <#message_ty as #crate_::__private::Deserialize>::deserialize(reader.0, reader.1);
                    async move {
                        match res {
                            Ok((msg, bytes_read, fds_read)) => {
                                match msg {
                                    #(#match_items),*
                                }
                            },
                            Err(e) => (Err(e.into()), 0, 0),
                        }
                    }
                }
            }
        };
    }.into()
}

/// Generate `InterfaceMessageDispatch` and `Object` impls for an enum type.
///
/// Generates a InterfaceMessageDispatch that dispatches to the
/// InterfaceMessageDispatch implemented by the enum variants.
///
/// Depending on the setting of `context`, this can be used to generate either a
/// generic or a concrete implementation of InterfaceMessageDispatch.
///
/// If `context` is not set, the generated implementation will be generic over
/// the the context type (i.e. the type parameter `Ctx` of
/// `InterfaceMessageDispatch`). If your object types are generic over the
/// context type too, then you must name it `Ctx`. i.e. if you wrote `impl<Ctx>
/// InterfaceMessageDispatch<Ctx> for Variant<Ctx>`, for some variant `Variant`,
/// then the generic parameter has to be called `Ctx` on the enum too.
/// If not, `Ctx` cannot appear in the generic parameters.
///
/// If `context` is set, the generated implementation will be an impl of
/// `InterfaceMessageDispatch<$context>`.
///
/// All variants' InterfaceMessageDispatch impls must have the same error type.
///
/// This also derives `impl From<Variant> for Enum` for each of the variants
///
/// # Attributes
///
/// Accept attributes in the form of `#[wayland(...)]`.
///
/// * `crate` - The path to the `wl_server` crate. "wl_server" by default.
/// * `context`
#[proc_macro_derive(InterfaceMessageDispatch, attributes(wayland))]
pub fn interface_message_dispatch_for_enum(
    orig_item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let orig_item = syn::parse_macro_input!(orig_item as syn::DeriveInput);
    #[derive(FromDeriveInput)]
    #[darling(attributes(wayland))]
    struct Enum {
        #[darling(default, rename = "crate")]
        crate_:   Option<syn::LitStr>,
        context:  Option<syn::LitStr>,
        ident:    syn::Ident,
        generics: syn::Generics,
        data:     darling::ast::Data<syn::Variant, darling::util::Ignored>,
    }
    let item = Enum::from_derive_input(&orig_item).unwrap();
    let darling::ast::Data::Enum(body) = &item.data else {
        return syn::Error::new_spanned(&orig_item, "Enum expected")
            .to_compile_error()
            .into()
    };
    let ident = &item.ident;
    let context = item
        .context
        .map(|c| syn::parse_str::<syn::Type>(&c.value()).unwrap());
    let has_ctx = item
        .generics
        .type_params()
        .any(|param| param.ident == "Ctx");
    let context_param = if let Some(context) = &context {
        quote! { #context }
    } else {
        quote! { Ctx }
    };
    let (impl_generics0, ty_generics, where_clause0) = item.generics.split_for_impl();
    let impl_generics = if has_ctx || context.is_some() {
        impl_generics0.to_token_stream()
    } else {
        let mut generics = item.generics.clone();
        generics.params.push(syn::parse_quote! { Ctx });
        generics.split_for_impl().0.to_token_stream()
    };
    let crate_: syn::Path = item.crate_.map_or_else(
        || syn::parse_str("::wl_server").unwrap(),
        |s| syn::parse_str(&s.value()).unwrap(),
    );
    let Some(first_var) = body.iter().next() else { return syn::Error::new_spanned(
        &orig_item,
        "Enum must have at least one variant",
    ).to_compile_error().into()};
    let syn::Fields::Unnamed(first_var) = &first_var.fields else { return syn::Error::new_spanned(
        first_var,
        "Enum must have one variant"
    ).to_compile_error().into() };
    let Some(first_var) = first_var.unnamed.first() else { return syn::Error::new_spanned(
        first_var,
        "Enum variant must have at least one field",
    ).to_compile_error().into()};
    let var = body.iter().map(|v| {
        if v.discriminant.is_some() {
            quote! {
                compile_error!("Enum discriminant not supported");
            }
        } else if v.fields.len() != 1 {
            quote! { compile_error!("Enum variant must have a single field"); }
        } else if let syn::Fields::Unnamed(fields) = &v.fields {
            let ty_ = &fields.unnamed.first().unwrap().ty;
            let ident = &v.ident;
            quote! {
                Self::#ident(f) => {
                    <#ty_ as #crate_::__private::InterfaceMessageDispatch<#context_param>>::dispatch(f, ctx, object_id, reader).await
                }
            }
        } else {
            quote! { compile_error!("Enum variant must have a single unnamed field"); }
        }
    });
    let froms = body.iter().map(|v| {
        let syn::Fields::Unnamed(fields) = &v.fields else { panic!() };
        let ty_ = &fields.unnamed.first().unwrap().ty;
        let v = &v.ident;
        quote! {
            impl #impl_generics0 From<#ty_> for #ident #ty_generics #where_clause0 {
                fn from(f: #ty_) -> Self {
                    #ident::#v(f)
                }
            }
        }
    });
    let casts = body.iter().map(|v| {
        let syn::Fields::Unnamed(fields) = &v.fields else { panic!() };
        let ty_ = &fields.unnamed.first().unwrap().ty;
        let v = &v.ident;
        quote! {
            #ident::#v(f) => <#ty_ as #crate_::objects::Object<#context_param>>::cast(f)
        }
    });
    let interfaces = body.iter().map(|v| {
        let syn::Fields::Unnamed(fields) = &v.fields else { panic!() };
        let ty_ = &fields.unnamed.first().unwrap().ty;
        let v = &v.ident;
        quote! {
            Self::#v(f) => <#ty_ as #crate_::objects::Object<#context_param>>::interface(f),
        }
    });
    let disconnects = body.iter().map(|v| {
        let syn::Fields::Unnamed(fields) = &v.fields else { panic!() };
        let ty_ = &fields.unnamed.first().unwrap().ty;
        let v = &v.ident;
        quote! {
            Self::#v(f) => <#ty_ as #crate_::objects::Object<#context_param>>::on_disconnect(f, ctx),
        }
    });
    let additional_bounds = body.iter().enumerate().map(|(i, v)| {
        if let syn::Fields::Unnamed(fields) = &v.fields {
            let ty_ = &fields.unnamed.first().unwrap().ty;
            if i != 0 {
                quote! {
                    #ty_: #crate_::__private::InterfaceMessageDispatch<#context_param, Error = <#first_var as #crate_::__private::InterfaceMessageDispatch<#context_param>>::Error>,
                }
            } else {
                quote! {
                    #ty_: #crate_::__private::InterfaceMessageDispatch<#context_param>,
                }
            }
        } else {
            quote! {}
        }
    });
    let where_clause = if let Some(where_clause) = where_clause0 {
        let mut where_clause = where_clause.clone();
        if !where_clause.predicates.trailing_punct() {
            where_clause
                .predicates
                .push_punct(<syn::Token![,]>::default());
        }
        quote! {
            #where_clause
            #(#additional_bounds)*
        }
    } else {
        quote! {
            where
                #(#additional_bounds)*
        }
    };
    let ctx_lifetime_bound = if context.is_some() {
        quote! {}
    } else {
        quote! { Ctx: 'a }
    };
    quote! {
        impl #impl_generics #crate_::__private::InterfaceMessageDispatch<#context_param> for #ident #ty_generics
        #where_clause {
            type Error = <#first_var as #crate_::__private::InterfaceMessageDispatch<#context_param>>::Error;
            type Fut<'a> = impl ::std::future::Future<Output = (::std::result::Result<(), Self::Error>, usize, usize)> + 'a
            where
                Self: 'a,
                #ctx_lifetime_bound;
            fn dispatch<'a>(
                &'a self,
                ctx: &'a mut #context_param,
                object_id: u32,
                reader: (&'a [u8], &'a [::std::os::unix::io::RawFd]),
            ) -> Self::Fut<'a>
            {
                async move {
                    match self {
                        #(#var)*
                    }
                }
            }
        }
        impl #impl_generics #crate_::objects::Object<#context_param> for #ident #ty_generics #where_clause {
            type Request<'a> = ::std::convert::Infallible;
            #[inline]
            fn interface(&self) -> &'static str {
                match self {
                    #(#interfaces)*
                }
            }
            #[inline]
            fn on_disconnect(&mut self, ctx: &mut #context_param) {
                match self {
                    #(#disconnects)*
                }
            }
            #[inline]
            fn cast<T: 'static>(&self) -> Option<&T> {
                match self {
                    #(#casts),*
                }
            }
        }
        #(#froms)*
    }
    .into()
}

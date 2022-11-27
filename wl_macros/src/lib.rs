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
    generics:     syn::Generics,
    trait_:       syn::Path,
    self_ty:      syn::Type,
    items:        Vec<DispatchItem>,
    error:        syn::Type,
    has_lifetime: bool,
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
        let mut has_lifetime = false;
        for item in &input.items {
            match item {
                syn::ImplItem::Method(method) => {
                    let mut args = Vec::new();
                    for arg in method.sig.inputs.iter().skip(3) {
                        // Ignore the first 3 arguments: self, ctx, object_id
                        match arg {
                            syn::FnArg::Receiver(_) => die!(&arg => "multiple receivers"),
                            syn::FnArg::Typed(patty) => {
                                if let syn::Pat::Ident(pat) = &*patty.pat {
                                    args.push(pat.ident.clone());
                                } else {
                                    die!(&patty.pat =>
                                        "Argument must be a simple identifier"
                                    );
                                }
                                let type_ = &patty.ty;
                                match &**type_ {
                                    syn::Type::Path(type_path) => {
                                        if let Some(segment) = type_path.path.segments.last() {
                                            if segment.ident == "Str" {
                                                has_lifetime = true;
                                            }
                                        }
                                    },
                                    syn::Type::Reference(_) => {
                                        has_lifetime = true;
                                    },
                                    _ => die!(type_ => "Unexpected argument type"),
                                }
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
            has_lifetime,
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

/// Generate `Object` impls for types that implement `RequestDispatch` for a
/// certain interface. Should be attached to `RequestDispatch` impls.
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
/// * `interface` - The interface name. By default, this attribute finds the
///   parent of the `RequestDispatch` trait, i.e.
///   `wl_buffer::v1::RequestDispatch` will become `wl_buffer::v1`; then attach
///   `::NAME` to it as the interface.
/// * `on_disconnect` - The function to call when the client disconnects. Used
///   for the [`wl_server::objects::Object::on_disconnect`] impl.
/// * `crate` - The path to the `wl_server` crate. "wl_server" by default.
#[proc_macro_attribute]
pub fn wayland_object(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    use heck::ToPascalCase;
    use quote::format_ident;
    #[derive(FromMeta)]
    struct Attributes {
        #[darling(default)]
        message:       Option<syn::Path>,
        #[darling(default, rename = "crate")]
        crate_:        Option<syn::LitStr>,
        #[darling(default)]
        interface:     Option<syn::LitStr>,
        #[darling(default)]
        on_disconnect: Option<syn::LitStr>,
    }
    let stream = item.clone();
    let orig_item = syn::parse_macro_input!(item as syn::ItemImpl);
    let item: DispatchImpl = syn::parse_macro_input!(stream);
    let attr: syn::AttributeArgs = syn::parse_macro_input!(attr);
    let attr = Attributes::from_list(&attr).unwrap();
    // The mod path
    let mod_ = {
        let mut trait_ = item.trait_.clone();
        trait_.segments.pop();
        trait_
    };

    let message_ty = unwrap!(attr.message.map_or_else(
        || {
            let mut mod_ = mod_.clone();
            let last_seg = item.trait_.segments.last().ok_or_else(|| {
                syn::Error::new_spanned(&item.trait_, "Trait path must not be empty")
            })?;

            let last_ident = last_seg.ident.to_string();
            if last_ident.ends_with("Dispatch") {
                let message_ty_last =
                    quote::format_ident!("{}", last_ident.trim_end_matches("Dispatch"));
                mod_.segments.push(syn::PathSegment::from(message_ty_last));
                Ok(mod_)
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

    // Generate Object impl.
    let DispatchImpl {
        generics,
        trait_,
        self_ty,
        items,
        error,
        has_lifetime,
    } = item;

    let Some(last_seg_args) = trait_
        .segments
        .last()
        .map(|s| &s.arguments) else
    {
        return syn::Error::new_spanned(&trait_, "Trait path must not be empty").to_compile_error().into();
    };
    let ctx = match last_seg_args {
        syn::PathArguments::Parenthesized(_) | syn::PathArguments::None =>
            return syn::Error::new_spanned(last_seg_args, "Trait must have a single type parameter")
                .to_compile_error()
                .into(),
        syn::PathArguments::AngleBracketed(ref args) => {
            if args.args.len() != 1 {
                return syn::Error::new_spanned(last_seg_args, "Trait must have a single type parameter")
                    .to_compile_error()
                    .into()
            }
            match args.args[0] {
                syn::GenericArgument::Type(syn::Type::Path(ref path)) => path.path.clone(),
                _ =>
                    return syn::Error::new_spanned(
                        last_seg_args,
                        "Trait must have a single type parameter",
                    )
                    .to_compile_error()
                    .into(),
            }
        },
    };
    let our_trait: syn::Path = syn::parse_quote! { #crate_::objects::Object #last_seg_args };
    let where_clause = generics.where_clause.as_ref();

    let match_items2 = items.iter().map(|item| {
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
                (#trait_::#ident(self, ctx, object_id, #(msg.#args),*).await, bytes, fds)
            }
        }
    });
    let message_lifetime = if has_lifetime {
        quote! { <'a> }
    } else {
        quote! {}
    };
    let interface_tokens = if let Some(interface) = attr.interface {
        let interface = interface.value();
        quote! {
            #interface
        }
    } else {
        quote! {
            #mod_ NAME
        }
    };
    let on_disconnect = if let Some(on_disconnect) = attr.on_disconnect {
        let on_disconnect = syn::Ident::new(&on_disconnect.value(), on_disconnect.span());
        quote! {
            fn on_disconnect(&mut self, ctx: &mut Ctx) {
                #on_disconnect(self, ctx)
            }
        }
    } else {
        quote! {}
    };

    quote! {
        #orig_item
        const _: () = {
            impl #generics #our_trait for #self_ty #where_clause {
                type Request<'a> = #message_ty #message_lifetime where #ctx: 'a, Self: 'a;
                type Error = #error;
                type Fut<'a> = impl ::std::future::Future<Output = (Result<(), Self::Error>, usize, usize)> + 'a
                where
                    Self: 'a,
                    #ctx: 'a;

                fn interface(&self) -> &'static str {
                    #interface_tokens
                }

                #[inline]
                fn dispatch<'a>(&'a self, ctx: &'a mut #ctx, object_id: u32, msg: Self::Request<'a>) -> Self::Fut<'a> {
                    let (bytes, fds) = (
                        <#message_ty as #crate_::__private::Serialize>::len(&msg) as usize,
                        <#message_ty as #crate_::__private::Serialize>::nfds(&msg) as usize,
                    );
                    async move {
                        match msg {
                            #(#match_items2),*
                        }
                    }
                }
                #on_disconnect
            }
        };
    }.into()
}

/// Generate `Object` impls for an enum type.
///
/// Generates a Object impl that dispatches to the
/// Object implemented by the enum variants.
///
/// Depending on the setting of `context`, this can be used to generate either a
/// generic or a concrete implementation of Object.
///
/// If `context` is not set, the generated implementation will be generic over
/// the the context type (i.e. the type parameter `Ctx` of
/// `Object`). If your object types are generic over the
/// context type too, then you must name it `Ctx`. i.e. if you wrote `impl<Ctx>
/// Object<Ctx> for Variant<Ctx>`, for some variant `Variant`,
/// then the generic parameter has to be called `Ctx` on the enum too.
/// If not, `Ctx` cannot appear in the generic parameters.
///
/// If `context` is set, the generated implementation will be an impl of
/// `Object<$context>`.
///
/// All variants' Object impls must have the same error type.
///
/// This also derives `impl From<Variant> for Enum` for each of the variants
///
/// # Attributes
///
/// Accept attributes in the form of `#[wayland(...)]`.
///
/// * `crate` - The path to the `wl_server` crate. "wl_server" by default.
/// * `context`
#[proc_macro_derive(Object, attributes(wayland))]
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
    let var2 = body.iter().map(|v| {
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
                    let msg = match #crate_::__private::Deserialize::deserialize(msg.0, msg.1) {
                        Ok(msg) => msg,
                        Err(e) => return (Err(e.into()), 0, 0),
                    };
                    let (res, bytes_read, fds_read) =
                        <#ty_ as #crate_::objects::Object<#context_param>>::dispatch(f, ctx, object_id, msg).await;
                    (res.map_err(Into::into), bytes_read, fds_read)
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
    let additional_bounds2 = body.iter().enumerate().map(|(i, v)| {
        if let syn::Fields::Unnamed(fields) = &v.fields {
            let ty_ = &fields.unnamed.first().unwrap().ty;
            if i != 0 {
                quote! {
                    #ty_: #crate_::objects::Object<#context_param, Error = <#first_var as #crate_::objects::Object<#context_param>>::Error>,
                }
            } else {
                quote! {
                    #ty_: #crate_::objects::Object<#context_param>,
                }
            }
        } else {
            quote! {}
        }
    });
    let where_clause2 = if let Some(where_clause) = where_clause0 {
        let mut where_clause = where_clause.clone();
        if !where_clause.predicates.trailing_punct() {
            where_clause
                .predicates
                .push_punct(<syn::Token![,]>::default());
        }
        quote! {
            #where_clause
            <#first_var as #crate_::objects::Object<#context_param>>::Error: From<#crate_::__private::DeserError>,
            #(#additional_bounds2)*
        }
    } else {
        quote! {
            where
                <#first_var as #crate_::objects::Object<#context_param>>::Error: From<#crate_::__private::DeserError>,
                #(#additional_bounds2)*
        }
    };
    let ctx_lifetime_bound = if context.is_some() {
        quote! {}
    } else {
        quote! { Ctx: 'a }
    };
    quote! {
        impl #impl_generics #crate_::objects::Object<#context_param> for #ident #ty_generics #where_clause2 {
            type Error = <#first_var as #crate_::objects::Object<#context_param>>::Error; // TODO
            type Fut<'a> = impl ::std::future::Future<Output = (::std::result::Result<(), Self::Error>, usize, usize)> + 'a
            where
                Self: 'a, #ctx_lifetime_bound;
            type Request<'a> = (&'a [u8], &'a [::std::os::unix::io::RawFd]) where Self: 'a, #ctx_lifetime_bound;
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
            fn dispatch<'a>(
                &'a self,
                ctx: &'a mut #context_param,
                object_id: u32,
                msg: Self::Request<'a>,
            ) -> Self::Fut<'a> {
                async move {
                    match self {
                        #(#var2),*
                    }
                }
            }
        }
        #(#froms)*
    }
    .into()
}

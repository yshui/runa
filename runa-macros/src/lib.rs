extern crate proc_macro;

use std::collections::HashMap;

use darling::{FromDeriveInput, FromMeta};
use quote::{quote, ToTokens};
use syn::{
    parse::Parse,
    visit_mut::{self, VisitMut},
};

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
                    for arg in method.sig.inputs.iter().skip(2) {
                        // Ignore the first 3 arguments: self, ctx, object_id
                        match arg {
                            syn::FnArg::Receiver(_) => die!(&arg => "Unexpected receivers"),
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

/// Remove unconstrained type parameters from a generics, where clause is not
/// supported and will be removed.
fn filter_generics(generics: &mut syn::Generics, ty: &mut syn::Type) -> Result<(), syn::Error> {
    let mut idents_used = HashMap::new();
    let mut lifetimes_used = HashMap::new();
    let mut consts_used = HashMap::new();
    for param in &generics.params {
        match param {
            syn::GenericParam::Type(type_param) => {
                idents_used.insert(type_param.ident.clone(), false);
            },
            syn::GenericParam::Lifetime(lifetime_param) => {
                lifetimes_used.insert(lifetime_param.lifetime.clone(), false);
            },
            syn::GenericParam::Const(const_param) => {
                consts_used.insert(const_param.ident.clone(), false);
            },
        }
    }
    struct ConstrainedGenericParams {
        idents: HashMap<syn::Ident, bool>,
        consts: HashMap<syn::Ident, bool>,
        error:  Option<syn::Error>,
    }

    impl VisitMut for ConstrainedGenericParams {
        fn visit_type_path_mut(&mut self, i: &mut syn::TypePath) {
            // `<T as Trait>::T2<T3>` is not a use of `T` or `T3`, so ignore
            // types with qself
            if i.qself.is_none() {
                if let Some(ident) = i.path.get_ident() {
                    // `T`
                    if let Some(used) = self.idents.get_mut(ident) {
                        *used = true;
                    }
                } else if let Some(segment) = i
                    .path
                    .segments
                    .iter_mut()
                    .find(|seg| !seg.arguments.is_empty())
                {
                    // `a::b::c::X<Args..>`, `Args..` can contain uses of parameters
                    visit_mut::visit_path_arguments_mut(self, &mut segment.arguments);
                }
                // `a::b::c::X`, `X` cannot be a type parameter
            }
        }
    }

    let mut constrained = ConstrainedGenericParams {
        idents: idents_used,
        consts: consts_used,
        error:  None,
    };
    match ty {
        syn::Type::Path(syn::TypePath { qself: None, path }) => {
            if let Some(segment) = path.segments.last_mut() {
                visit_mut::visit_path_arguments_mut(&mut constrained, &mut segment.arguments);
            }
        },
        _ =>
            return Err(syn::Error::new_spanned(
                ty,
                "wayland_object attribute must be used on a base type",
            )),
    }

    if let Some(error) = constrained.error {
        return Err(error)
    }

    //eprintln!("Used idents: {:?}", idents_used);
    //eprintln!("Used lifetimes: {:?}", lifetimes_used);
    //eprintln!("Used consts: {:?}", consts_used);
    generics.params = std::mem::take(&mut generics.params)
        .into_iter()
        .filter(|param| match param {
            syn::GenericParam::Type(type_param) =>
                *constrained.idents.get(&type_param.ident).unwrap(),
            syn::GenericParam::Lifetime(_) => true,
            syn::GenericParam::Const(const_param) =>
                *constrained.consts.get(&const_param.ident).unwrap(),
        })
        .collect();
    generics.where_clause = None;
    Ok(())
}

#[proc_macro_attribute]
pub fn wayland_object(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    use heck::ToPascalCase;
    use quote::format_ident;

    struct StateType {
        ty:           syn::Type,
        where_clause: Option<syn::WhereClause>,
    }

    impl Parse for StateType {
        fn parse(input: syn::parse::ParseStream) -> Result<Self, syn::Error> {
            let ty = input.parse()?;
            let where_clause = input.parse()?;
            Ok(Self { ty, where_clause })
        }
    }

    impl darling::FromMeta for StateType {
        fn from_string(value: &str) -> darling::Result<Self> {
            Ok(syn::parse_str(value)?)
        }
    }

    #[derive(FromMeta)]
    struct Attributes {
        #[darling(default)]
        message:       Option<syn::Path>,
        #[darling(default, rename = "crate")]
        crate_:        Option<syn::Path>,
        #[darling(default)]
        interface:     Option<syn::LitStr>,
        #[darling(default)]
        on_disconnect: Option<syn::Ident>,
        #[darling(default)]
        state:         Option<StateType>,
        #[darling(default)]
        state_init:    Option<syn::Expr>,
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
    let crate_: syn::Path = attr
        .crate_
        .unwrap_or_else(|| syn::parse_str("::runa_core").unwrap());

    // Generate Object impl.
    let DispatchImpl {
        generics,
        trait_,
        mut self_ty,
        items,
        error,
        has_lifetime,
    } = item;

    let Some(last_seg_args) = trait_.segments.last().map(|s| &s.arguments) else {
        return syn::Error::new_spanned(&trait_, "Trait path must not be empty")
            .to_compile_error()
            .into();
    };
    let ctx = match last_seg_args {
        syn::PathArguments::Parenthesized(_) | syn::PathArguments::None =>
            return syn::Error::new_spanned(last_seg_args, "Trait must have a single type parameter")
                .to_compile_error()
                .into(),
        syn::PathArguments::AngleBracketed(ref args) => {
            if args.args.len() != 1 {
                return syn::Error::new_spanned(
                    last_seg_args,
                    "Trait must have a single type parameter",
                )
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
                (<Self as #trait_>::#ident(ctx, object_id, #(msg.#args),*).await, bytes, fds)
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
        quote! {
            fn on_disconnect(&mut self, server_ctx: &mut Ctx::ServerContext, state: &mut dyn ::std::any::Any) {
                #on_disconnect(self, server_ctx, state.downcast_mut().unwrap())
            }
        }
    } else {
        quote! {}
    };
    let singleton_state_type = if let Some(state) = attr.state.as_ref() {
        let state_ty = &state.ty;
        quote! {
            #state_ty
        }
    } else {
        quote! {
            ()
        }
    };

    let state_init = if let Some(state_init) = attr.state_init {
        quote! {
            #state_init
        }
    } else if attr.state.is_some() {
        quote! {
            Default::default()
        }
    } else {
        quote! {}
    };

    let mut filtered_generics = generics.clone();
    match filter_generics(&mut filtered_generics, &mut self_ty) {
        Err(e) => return e.to_compile_error().into(),
        Ok(g) => g,
    };
    let state_where = attr.state.as_ref().map(|state| &state.where_clause);
    let log = if cfg!(feature = "tracing") {
        quote! {
            tracing::debug!(target: "wl_io::deser", "Dispatching {:?}, interface {}", msg, #interface_tokens);
        }
    } else {
        quote!()
    };
    quote! {
        #orig_item
        const _: () = {
            impl #filtered_generics #crate_::objects::MonoObject for #self_ty #state_where {
                type SingletonState = #singleton_state_type;
                const INTERFACE: &'static str = #interface_tokens;
                #[inline]
                fn new_singleton_state() -> Self::SingletonState {
                    #state_init
                }
            }

            impl #generics #our_trait for #self_ty #where_clause {
                type Request<'a> = #message_ty #message_lifetime where #ctx: 'a;
                type Error = #error;
                type Fut<'a> = impl ::std::future::Future<Output = (Result<(), Self::Error>, usize, usize)> + 'a
                where
                    #ctx: 'a;

                #[inline]
                fn dispatch<'a>(ctx: &'a mut #ctx, object_id: u32, msg: Self::Request<'a>) -> Self::Fut<'a> {
                    let (bytes, fds) = (
                        <#message_ty as #crate_::__private::Serialize>::len(&msg) as usize,
                        <#message_ty as #crate_::__private::Serialize>::nfds(&msg) as usize,
                    );
                    #log
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
/// context type too, then you must name that generic parameter `Ctx` too.
///
/// If `context` is set, the generated implementation will be an impl of
/// `Object<$context>`.
///
/// All variants' Object impls must have the same error type.
///
/// This also derives `impl From<Variant> for Enum` for each of the variants
///
/// # Examples
///
/// For the generic case:
///
/// ```ignore
/// #[derive(Object)]
/// pub enum Objects<Ctx> { // this must be called `Ctx`
///     Display(MyDisplayObject<Ctx>),
/// }
/// ```
///
/// This will generate:
///
/// ```ignore
/// impl<Ctx> Object<Ctx> for Objects<Ctx> {
///     // ..
/// }
/// impl<Ctx> From<MyDisplayObject<Ctx>> for Object<Ctx> {
///     // ..
/// }
/// ```
///
/// If the generic parameter is named something else, such as `T`, this will be
/// generated:
///
/// ```ignore
/// impl<Ctx, T> Object<Ctx> for Objects<T> {
///     // ..
/// }
/// ```
///
/// # Attributes
///
/// Accept attributes in the form of `#[wayland(...)]`.
///
/// * `crate` - The path to the `runa-core` crate. "runa_core" by default.
/// * `context` - See above
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
        || syn::parse_str("::runa_core").unwrap(),
        |s| syn::parse_str(&s.value()).unwrap(),
    );
    let Some(first_var) = body.iter().next() else {
        return syn::Error::new_spanned(&orig_item, "Enum must have at least one variant")
            .to_compile_error()
            .into()
    };
    let syn::Fields::Unnamed(first_var) = &first_var.fields else {
        return syn::Error::new_spanned(first_var, "Enum must have one variant")
            .to_compile_error()
            .into()
    };
    let Some(first_var) = first_var.unnamed.first() else {
        return syn::Error::new_spanned(first_var, "Enum variant must have at least one field")
            .to_compile_error()
            .into()
    };
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
                Self::#ident(_) => {
                    let msg = match #crate_::__private::Deserialize::deserialize(msg.0, msg.1) {
                        Ok(msg) => msg,
                        Err(e) => return (Err(e.into()), 0, 0),
                    };
                    let (res, bytes_read, fds_read) =
                        <#ty_ as #crate_::objects::Object<#context_param>>::dispatch(ctx, object_id, msg).await;
                    (res.map_err(Into::into), bytes_read, fds_read)
                }
            }
        } else {
            quote! { compile_error!("Enum variant must have a single unnamed field"); }
        }
    });
    let froms = body.iter().map(|v| {
        let syn::Fields::Unnamed(fields) = &v.fields else {
            panic!()
        };
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
        let syn::Fields::Unnamed(_) = &v.fields else {
            panic!()
        };
        let v = &v.ident;
        quote! {
            #ident::#v(f) => (f as &dyn ::std::any::Any).downcast_ref()
        }
    });
    let cast_muts = body.iter().map(|v| {
        let syn::Fields::Unnamed(_) = &v.fields else {
            panic!()
        };
        let v = &v.ident;
        quote! {
            #ident::#v(f) => (f as &mut dyn ::std::any::Any).downcast_mut()
        }
    });
    let interfaces = body.iter().map(|v| {
        let syn::Fields::Unnamed(fields) = &v.fields else {
            panic!()
        };
        let ty_ = &fields.unnamed.first().unwrap().ty;
        let v = &v.ident;
        quote! {
            Self::#v(_) => <#ty_ as #crate_::objects::MonoObject>::INTERFACE,
        }
    });
    let disconnects = body.iter().map(|v| {
        let syn::Fields::Unnamed(fields) = &v.fields else { panic!() };
        let ty_ = &fields.unnamed.first().unwrap().ty;
        let v = &v.ident;
        quote! {
            Self::#v(f) => <#ty_ as #crate_::objects::Object<#context_param>>::on_disconnect(f, server_ctx, state),
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
    let singleton_states = body.iter().map(|v| {
        let syn::Fields::Unnamed(fields) = &v.fields else { panic!() };
        let ty_ = &fields.unnamed.first().unwrap().ty;
        let v = &v.ident;
        quote! {
            Self::#v(_) => Box::new(<#ty_ as #crate_::objects::MonoObject>::new_singleton_state()) as _,
        }
    });
    let type_ids = body.iter().map(|v| {
        let syn::Fields::Unnamed(fields) = &v.fields else {
            panic!()
        };
        let ty_ = &fields.unnamed.first().unwrap().ty;
        let v = &v.ident;
        quote! {
            Self::#v(_) => ::std::any::TypeId::of::<#ty_>(),
        }
    });
    quote! {
        impl #impl_generics #crate_::objects::AnyObject for #ident #ty_generics #where_clause2 {
            #[inline]
            fn interface(&self) -> &'static str {
                match self {
                    #(#interfaces)*
                }
            }
            #[inline]
            fn cast<T: 'static>(&self) -> Option<&T> {
                use ::std::any::Any;
                if let Some(obj) = (self as &dyn Any).downcast_ref::<T>() {
                    Some(obj)
                } else {
                    match self {
                        #(#casts),*
                    }
                }
            }
            #[inline]
            fn cast_mut<T: 'static>(&mut self) -> Option<&mut T> {
                use ::std::any::Any;
                if (self as &dyn Any).is::<T>() {
                    // Safety: we just checked that the type is correct
                    Some(unsafe { (self as &mut dyn Any).downcast_mut::<T>().unwrap_unchecked() })
                } else {
                    match self {
                        #(#cast_muts),*
                    }
                }
            }

            #[inline]
            fn new_singleton_state(&self) -> Box<dyn ::std::any::Any> {
                match self {
                    #(#singleton_states)*
                }
            }
            #[inline]
            fn type_id(&self) -> ::std::any::TypeId {
                match self {
                    #(#type_ids)*
                }
            }
        }
        impl #impl_generics #crate_::objects::Object<#context_param> for #ident #ty_generics #where_clause2 {
            type Error = <#first_var as #crate_::objects::Object<#context_param>>::Error; // TODO
            type Fut<'a> = impl ::std::future::Future<Output = (::std::result::Result<(), Self::Error>, usize, usize)> + 'a
            where
                #ctx_lifetime_bound;
            type Request<'a> = (&'a [u8], &'a [::std::os::unix::io::RawFd]) where Self: 'a, #ctx_lifetime_bound;
            #[inline]
            fn on_disconnect(
                &mut self,
                server_ctx: &mut <#context_param as #crate_::client::traits::Client>::ServerContext,
                state: &mut dyn ::std::any::Any
            ) {
                match self {
                    #(#disconnects)*
                }
            }
            fn dispatch<'a>(
                ctx: &'a mut #context_param,
                object_id: u32,
                msg: Self::Request<'a>,
            ) -> Self::Fut<'a> {
                async move {
                    use #crate_::client::traits::Store;
                    match ctx.objects().get::<Self>(object_id) {
                        Ok(obj) => {
                            // We are doing this weird dance here because if we do `if let
                            // Some(object) = object` then `object` will be dropped too late, and
                            // the borrow checker complains we are still borrowing `ctx`.
                            match &*obj {
                                #(#var2),*
                            }
                        },
                        Err(e) =>(Err(e.into()), 0, 0),
                    }
                }
            }
        }
        #(#froms)*
    }
    .into()
}

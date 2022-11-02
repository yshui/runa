extern crate proc_macro;
use darling::{FromDeriveInput, FromMeta, FromVariant};
use proc_macro_error::ResultExt;
use quote::quote;
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
    use quote::format_ident;
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
                type Fut<'a, 'b, D> = impl ::std::future::Future<Output = ::std::result::Result<(), Self::Error>> + 'a
                where
                    Self: 'a,
                    Ctx: 'a,
                    D: ::wl_io::traits::de::Deserializer<'b>,
                    'b: 'a;
                fn dispatch<'a, 'b: 'a, D: ::wl_io::traits::de::Deserializer<'b>>(
                    &'a self,
                    ctx: &'a mut Ctx,
                    object_id: u32,
                    mut reader: D,
                ) -> Self::Fut<'a, 'b, D>
                {
                    let msg: ::std::result::Result<#message_ty, _> =
                        ::wl_io::traits::de::Deserialize::deserialize(&mut reader);
                    async move {
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

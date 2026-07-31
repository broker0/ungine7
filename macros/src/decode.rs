//! Code generation for `#[derive(Decode)]`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

use crate::attrs::StructAttrs;
use crate::codegen;

pub fn derive(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let struct_attrs = StructAttrs::from_attrs(&input.attrs)?;

    let data = match &input.data {
        syn::Data::Struct(s) => s,
        _ => return Err(syn::Error::new_spanned(input, "Decode can only be derived for structs")),
    };

    let fields = codegen::extract_named_fields(data)?;
    let (impl_generics, impl_trait) = struct_attrs.endian.impl_tokens();
    let endian_placeholder = quote! { _ };
    let (field_names, decode_stmts) = codegen::gen_decode_stmts(fields, &endian_placeholder)?;

    Ok(quote! {
        impl #impl_generics ::u_io::Decode<#impl_trait> for #name {
            fn decode<__R: ::u_io::ReadPrimitives<#impl_trait>>(
                __reader: &mut __R,
            ) -> ::core::result::Result<Self, ::u_io::DecodeError> {
                #(#decode_stmts)*
                ::core::result::Result::Ok(Self { #(#field_names,)* })
            }
        }
    })
}

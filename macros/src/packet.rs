//! Code generation for `#[derive(Packet)]`.
//!
//! Generates `Decode<E>`, `Encode<E>`, and `BasicPacket` implementations
//! from a single derive with `#[packet(id = 0xNN, size = fixed(N) | dynamic, endian = "be")]`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Expr, LitInt, LitStr};

use crate::codegen;

/// Parsed `#[packet(...)]` struct-level attributes.
struct PacketAttrs {
    id: LitInt,
    size: PacketSizeAttr,
    endian: PacketEndian,
}

enum PacketSizeAttr {
    Fixed(Expr),
    Dynamic,
}

#[derive(Clone, Copy)]
enum PacketEndian {
    Be,
    Le,
}

impl PacketEndian {
    fn type_tokens(self) -> TokenStream {
        match self {
            PacketEndian::Be => quote! { ::u_io::BE },
            PacketEndian::Le => quote! { ::u_io::LE },
        }
    }
}

impl PacketAttrs {
    fn from_attrs(attrs: &[syn::Attribute]) -> syn::Result<Self> {
        let mut id: Option<LitInt> = None;
        let mut size: Option<PacketSizeAttr> = None;
        let mut endian = PacketEndian::Be;

        for attr in attrs {
            if !attr.path().is_ident("packet") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("id") {
                    let value: LitInt = meta.value()?.parse()?;
                    id = Some(value);
                    Ok(())
                } else if meta.path.is_ident("size") {
                    let stream = meta.value()?;
                    let kw: syn::Ident = stream.parse()?;
                    if kw == "dynamic" {
                        size = Some(PacketSizeAttr::Dynamic);
                    } else if kw == "fixed" {
                        let content;
                        syn::parenthesized!(content in stream);
                        let n: Expr = content.parse()?;
                        size = Some(PacketSizeAttr::Fixed(n));
                    } else {
                        return Err(syn::Error::new_spanned(
                            &kw,
                            format!("expected `fixed(N)` or `dynamic`, got `{kw}`"),
                        ));
                    }
                    Ok(())
                } else if meta.path.is_ident("endian") {
                    let value: LitStr = meta.value()?.parse()?;
                    endian = match value.value().as_str() {
                        "be" => PacketEndian::Be,
                        "le" => PacketEndian::Le,
                        other => {
                            return Err(syn::Error::new_spanned(
                                &value,
                                format!("unknown endian: \"{other}\", expected \"be\" or \"le\""),
                            ));
                        }
                    };
                    Ok(())
                } else {
                    Err(meta.error("unknown #[packet(...)] attribute"))
                }
            })?;
        }

        let id = id.ok_or_else(|| {
            syn::Error::new(proc_macro2::Span::call_site(), "#[packet(id = ...)] is required")
        })?;
        let size = size.ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "#[packet(size = fixed(N) | dynamic)] is required",
            )
        })?;

        Ok(Self { id, size, endian })
    }
}

pub fn derive(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let packet_attrs = PacketAttrs::from_attrs(&input.attrs)?;

    let data = match &input.data {
        syn::Data::Struct(s) => s,
        _ => return Err(syn::Error::new_spanned(input, "Packet can only be derived for structs")),
    };

    let fields = codegen::extract_named_fields(data)?;
    let endian_ty = packet_attrs.endian.type_tokens();

    // ── Decode / Encode bodies via shared codegen ──────────────────────
    let (field_names, decode_stmts) = codegen::gen_decode_stmts(fields, &endian_ty)?;
    let encode_stmts = codegen::gen_encode_stmts(fields, &endian_ty)?;

    // ── BasicPacket constants ────────────────────────────────────────────
    let id_lit = &packet_attrs.id;
    let size_tokens = match &packet_attrs.size {
        PacketSizeAttr::Fixed(n) => {
            quote! { ::u_io::PacketSize::Fixed(#n) }
        }
        PacketSizeAttr::Dynamic => {
            quote! { ::u_io::PacketSize::Dynamic }
        }
    };

    Ok(quote! {
        impl ::u_io::Decode<#endian_ty> for #name {
            fn decode<__R: ::u_io::ReadPrimitives<#endian_ty>>(
                __reader: &mut __R,
            ) -> ::core::result::Result<Self, ::u_io::DecodeError> {
                #(#decode_stmts)*
                ::core::result::Result::Ok(Self { #(#field_names,)* })
            }
        }

        impl ::u_io::Encode<#endian_ty> for #name {
            fn encode(&self, __writer: &mut ::u_io::BinaryWriter<#endian_ty>) {
                #(#encode_stmts)*
            }
        }

        impl ::u_io::BasicPacket for #name {
            const ID: u8 = #id_lit;
            const SIZE: ::u_io::PacketSize = #size_tokens;
        }
    })
}

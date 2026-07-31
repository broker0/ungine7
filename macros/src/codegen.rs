//! Shared code-generation helpers for `Decode`, `Encode`, and `Packet` derives.
//!
//! Each helper accepts an `endian_ty` token stream that is spliced into the
//! generated trait bounds (`Decode<#endian_ty>`, `Encode<#endian_ty>`).
//! Callers pass either `_` (for standalone derives where the compiler infers
//! the type parameter) or a concrete type like `::u_io::BE`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{DataStruct, Field, Fields, Token};

use crate::attrs::{FieldAttrs, FieldKind};

// ── Helpers ────────────────────────────────────────────────────────────────

/// Extract named fields from a struct, or emit a compile error.
pub fn extract_named_fields(data: &DataStruct) -> syn::Result<&Punctuated<Field, Token![,]>> {
    match &data.fields {
        Fields::Named(f) => Ok(&f.named),
        _ => Err(syn::Error::new_spanned(
            &data.fields,
            "only structs with named fields are supported",
        )),
    }
}

// ── Decode ─────────────────────────────────────────────────────────────────

/// Generate per-field `let field = ...;` decode statements.
///
/// Returns `(field_names, statements)` so the caller can build the final
/// `Self { field1, field2, … }` constructor.
pub fn gen_decode_stmts(
    fields: &Punctuated<Field, Token![,]>,
    endian_ty: &TokenStream,
) -> syn::Result<(Vec<syn::Ident>, Vec<TokenStream>)> {
    let mut names = Vec::new();
    let mut stmts = Vec::new();

    for field in fields {
        let ident = field.ident.as_ref().unwrap().clone();
        let ty = &field.ty;
        let fa = FieldAttrs::from_attrs(&field.attrs)?;

        let stmt = match &fa.kind {
            FieldKind::Normal => {
                quote! {
                    let #ident = <#ty as ::u_io::Decode<#endian_ty>>::decode(__reader)?;
                }
            }
            FieldKind::Pad(n) => {
                quote! {
                    __reader.skip(#n)?;
                    let #ident = ();
                }
            }
            FieldKind::Const(expr) => {
                let field_name = ident.to_string();
                quote! {
                    let #ident = <#ty as ::u_io::Decode<#endian_ty>>::decode(__reader)?;
                    if #ident != (#expr) {
                        return ::core::result::Result::Err(
                            ::u_io::DecodeError::Other(
                                ::std::format!(
                                    "field `{}`: expected {:?}, got {:?}",
                                    #field_name, #expr, #ident
                                )
                            ),
                        );
                    }
                }
            }
            FieldKind::Skip => {
                quote! {
                    let #ident = ::core::default::Default::default();
                }
            }
            FieldKind::CustomDecode(path) => {
                quote! {
                    let #ident = #path(__reader)?;
                }
            }
            FieldKind::CustomBoth(path) => {
                quote! {
                    let #ident = #path::decode(__reader)?;
                }
            }
            FieldKind::CustomEncode(_) => {
                // encode_with only — standard decode.
                quote! {
                    let #ident = <#ty as ::u_io::Decode<#endian_ty>>::decode(__reader)?;
                }
            }
        };

        names.push(ident);
        stmts.push(stmt);
    }

    Ok((names, stmts))
}

// ── Encode ─────────────────────────────────────────────────────────────────

/// Generate per-field encode statements (`self.field → writer`).
pub fn gen_encode_stmts(
    fields: &Punctuated<Field, Token![,]>,
    endian_ty: &TokenStream,
) -> syn::Result<Vec<TokenStream>> {
    let mut stmts = Vec::new();

    for field in fields {
        let ident = field.ident.as_ref().unwrap();
        let ty = &field.ty;
        let fa = FieldAttrs::from_attrs(&field.attrs)?;

        let stmt = match &fa.kind {
            FieldKind::Normal => {
                quote! {
                    <#ty as ::u_io::Encode<#endian_ty>>::encode(&self.#ident, __writer);
                }
            }
            FieldKind::Pad(n) => {
                quote! {
                    __writer.put_bytes(0, #n);
                }
            }
            FieldKind::Const(expr) => {
                quote! {
                    let __const_val: #ty = #expr;
                    <#ty as ::u_io::Encode<#endian_ty>>::encode(&__const_val, __writer);
                }
            }
            FieldKind::Skip => {
                // Skip: write nothing.
                quote! { let _ = &self.#ident; }
            }
            FieldKind::CustomEncode(path) => {
                quote! {
                    #path(&self.#ident, __writer);
                }
            }
            FieldKind::CustomBoth(path) => {
                quote! {
                    #path::encode(&self.#ident, __writer);
                }
            }
            FieldKind::CustomDecode(_) => {
                // decode_with only — standard encode.
                quote! {
                    <#ty as ::u_io::Encode<#endian_ty>>::encode(&self.#ident, __writer);
                }
            }
        };
        stmts.push(stmt);
    }

    Ok(stmts)
}

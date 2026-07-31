//! Parsing of `#[binary(...)]` attributes for struct/field-level configuration.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Expr, LitInt, LitStr, Path};

// ── Struct-level #[binary(...)] ────────────────────────────────────────────

/// Byte-order mode parsed from `#[binary(endian = "...")]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    /// `#[binary(endian = "be")]` → `impl Decode<::u_io::BE>`
    Be,
    /// `#[binary(endian = "le")]` → `impl Decode<::u_io::LE>`
    Le,
    /// `#[binary(endian = "generic")]` or no attribute → `impl<E: ByteOrder> Decode<E>`
    Generic,
}

impl Endian {
    /// Returns `(impl_generics, trait_type)` token streams for use in
    /// generated `impl Decode<E>` / `impl Encode<E>` blocks.
    ///
    /// - `Be`      → `(∅, ::u_io::BE)`
    /// - `Le`      → `(∅, ::u_io::LE)`
    /// - `Generic` → `(<__E: ::u_io::ByteOrder>, __E)`
    pub fn impl_tokens(&self) -> (TokenStream, TokenStream) {
        match self {
            Endian::Be => (quote! {}, quote! { ::u_io::BE }),
            Endian::Le => (quote! {}, quote! { ::u_io::LE }),
            Endian::Generic => (
                quote! { <__E: ::u_io::ByteOrder> },
                quote! { __E },
            ),
        }
    }
}

/// Parsed struct-level `#[binary(...)]` attributes.
#[derive(Debug, Clone)]
pub struct StructAttrs {
    pub endian: Endian,
}

impl StructAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> syn::Result<Self> {
        let mut endian = Endian::Generic;

        for attr in attrs {
            if !attr.path().is_ident("binary") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("endian") {
                    let value: LitStr = meta.value()?.parse()?;
                    endian = match value.value().as_str() {
                        "be" => Endian::Be,
                        "le" => Endian::Le,
                        "generic" => Endian::Generic,
                        other => {
                            return Err(syn::Error::new_spanned(
                                &value,
                                format!("unknown endian: \"{other}\", expected \"be\", \"le\", or \"generic\""),
                            ));
                        }
                    };
                    Ok(())
                } else {
                    Err(meta.error("unknown #[binary(...)] attribute on struct"))
                }
            })?;
        }

        Ok(Self { endian })
    }
}

// ── Field-level #[binary(...)] ─────────────────────────────────────────────

/// What kind of wire element a field represents.
#[derive(Debug, Clone)]
pub enum FieldKind {
    /// Normal field: `T::decode(reader)` / `T::encode(&self.field, writer)`.
    Normal,

    /// `#[binary(pad = N)]` on a `()` field.
    /// Decode: `reader.skip(N)?`, Encode: `writer.put_bytes(0, N)`.
    Pad(usize),

    /// `#[binary(const_value = EXPR)]`.
    /// Decode: read `T`, validate == EXPR, return `BadConstant` on mismatch.
    /// Encode: write EXPR (ignore field value).
    Const(Expr),

    /// `#[binary(skip)]`.
    /// Decode: `Default::default()`, Encode: nothing.
    Skip,

    /// `#[binary(decode_with = "path")]` — custom decode, standard encode.
    CustomDecode(Path),

    /// `#[binary(encode_with = "path")]` — standard decode, custom encode.
    CustomEncode(Path),

    /// `#[binary(with = "path")]` — custom both.
    CustomBoth(Path),
}

/// Fully parsed field metadata.
#[derive(Debug, Clone)]
pub struct FieldAttrs {
    pub kind: FieldKind,
}

impl FieldAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> syn::Result<Self> {
        let mut primary_kind: Option<FieldKind> = None;
        let mut decode_with: Option<Path> = None;
        let mut encode_with: Option<Path> = None;

        for attr in attrs {
            if !attr.path().is_ident("binary") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("pad") {
                    if primary_kind.is_some() {
                        return Err(meta.error("conflicting #[binary(...)] attributes on field"));
                    }
                    let value: LitInt = meta.value()?.parse()?;
                    let n: usize = value.base10_parse()?;
                    primary_kind = Some(FieldKind::Pad(n));
                } else if meta.path.is_ident("const_value") {
                    if primary_kind.is_some() {
                        return Err(meta.error("conflicting #[binary(...)] attributes on field"));
                    }
                    let value: Expr = meta.value()?.parse()?;
                    primary_kind = Some(FieldKind::Const(value));
                } else if meta.path.is_ident("skip") {
                    if primary_kind.is_some() {
                        return Err(meta.error("conflicting #[binary(...)] attributes on field"));
                    }
                    primary_kind = Some(FieldKind::Skip);
                } else if meta.path.is_ident("with") {
                    if primary_kind.is_some() {
                        return Err(meta.error("conflicting #[binary(...)] attributes on field"));
                    }
                    let value: LitStr = meta.value()?.parse()?;
                    let path: Path = value.parse()?;
                    primary_kind = Some(FieldKind::CustomBoth(path));
                } else if meta.path.is_ident("decode_with") {
                    if decode_with.is_some() {
                        return Err(meta.error("duplicate #[binary(decode_with = ...)] on field"));
                    }
                    let value: LitStr = meta.value()?.parse()?;
                    decode_with = Some(value.parse()?);
                } else if meta.path.is_ident("encode_with") {
                    if encode_with.is_some() {
                        return Err(meta.error("duplicate #[binary(encode_with = ...)] on field"));
                    }
                    let value: LitStr = meta.value()?.parse()?;
                    encode_with = Some(value.parse()?);
                } else {
                    return Err(meta.error("unknown #[binary(...)] attribute on field"));
                }
                Ok(())
            })?;
        }

        if primary_kind.is_some() && (decode_with.is_some() || encode_with.is_some()) {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "conflicting #[binary(...)] attributes on field: do not combine `pad`/`const_value`/`skip`/`with` with `decode_with` or `encode_with`",
            ));
        }

        let kind = if let Some(kind) = primary_kind {
            kind
        } else {
            match (decode_with, encode_with) {
                (Some(d), Some(e)) if d == e => FieldKind::CustomBoth(d),
                (Some(_d), Some(_e)) => {
                    return Err(syn::Error::new(
                        proc_macro2::Span::call_site(),
                        "decode_with and encode_with with different paths: use #[binary(with = \"...\")] instead",
                    ));
                }
                (Some(d), None) => FieldKind::CustomDecode(d),
                (None, Some(e)) => FieldKind::CustomEncode(e),
                (None, None) => FieldKind::Normal,
            }
        };

        Ok(Self { kind })
    }
}

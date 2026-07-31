//! Code generation for `#[derive(WireEnum)]`.
//!
//! Generates `from_wire`, `to_wire`, `Display`, `Decode<E>`, `Encode<E>`.
//!
//! Supports `u8`, `u16`, and `u32` backed enums via `#[repr(...)]`.
//!
//! API differs depending on whether a `#[wire_enum(unknown)]` variant exists:
//! - **With unknown**: `from_wire(T) -> Self` (infallible)
//! - **Without unknown**: `from_wire(T) -> Result<Self, DecodeError>` (fallible)

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{DeriveInput, Fields, LitInt, LitStr};

/// Wire integer type parsed from `#[repr(u8|u16|u32)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    U8,
    U16,
    U32,
}

impl WireType {
    /// Parse from the enum's `#[repr(...)]` attributes.
    pub fn from_attrs(attrs: &[syn::Attribute]) -> syn::Result<Self> {
        for attr in attrs {
            if !attr.path().is_ident("repr") {
                continue;
            }
            let mut found: Option<WireType> = None;
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("u8") {
                    found = Some(WireType::U8);
                } else if meta.path.is_ident("u16") {
                    found = Some(WireType::U16);
                } else if meta.path.is_ident("u32") {
                    found = Some(WireType::U32);
                }
                Ok(())
            })?;
            if let Some(t) = found {
                return Ok(t);
            }
        }
        Err(syn::Error::new(
            Span::call_site(),
            "WireEnum requires #[repr(u8)], #[repr(u16)], or #[repr(u32)] on the enum",
        ))
    }

    /// The Rust type token (`u8`, `u16`, `u32`).
    pub fn type_token(self) -> TokenStream {
        match self {
            WireType::U8  => quote! { u8  },
            WireType::U16 => quote! { u16 },
            WireType::U32 => quote! { u32 },
        }
    }

    /// The `BinaryWriter` method (`put_u8`, `put_u16`, `put_u32`).
    pub fn put_method(self) -> syn::Ident {
        let name = match self {
            WireType::U8  => "put_u8",
            WireType::U16 => "put_u16",
            WireType::U32 => "put_u32",
        };
        syn::Ident::new(name, Span::call_site())
    }

    /// The `ReadPrimitives` method (`read_u8`, `read_u16`, `read_u32`).
    pub fn read_method(self) -> syn::Ident {
        let name = match self {
            WireType::U8  => "read_u8",
            WireType::U16 => "read_u16",
            WireType::U32 => "read_u32",
        };
        syn::Ident::new(name, Span::call_site())
    }

    /// Format specifier for Display's unknown arm (`02X`, `04X`, `08X`).
    pub fn fmt_spec(self) -> &'static str {
        match self {
            WireType::U8  => "02X",
            WireType::U16 => "04X",
            WireType::U32 => "08X",
        }
    }
}

// ── Parsed variant ─────────────────────────────────────────────────────────

/// A known variant: wire value + optional display string.
struct KnownVariant {
    ident: syn::Ident,
    value: LitInt,
    /// `Some("display text")` if provided, `None` → use variant name.
    display: Option<LitStr>,
}

impl KnownVariant {
    /// The `Display` token stream for this variant.
    fn display_tokens(&self) -> TokenStream {
        match &self.display {
            Some(s) => quote! { f.write_str(#s) },
            None => {
                let name = self.ident.to_string();
                quote! { f.write_str(#name) }
            }
        }
    }
}

// ── Attribute parsing ──────────────────────────────────────────────────────

/// Parse `#[wire_enum(...)]` on a variant.
///
/// Accepted forms:
/// - `#[wire_enum(VALUE)]`              — value only, display from variant name
/// - `#[wire_enum(VALUE, "text")]`      — value + explicit display string
/// - `#[wire_enum(unknown)]`            — catch-all unknown variant
enum ParsedVariantAttr {
    Value { value: LitInt, display: Option<LitStr> },
    Unknown,
}

impl syn::parse::Parse for ParsedVariantAttr {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        // "unknown" keyword
        if input.peek(syn::Ident) {
            let ident: syn::Ident = input.fork().parse()?;
            if ident == "unknown" {
                let _: syn::Ident = input.parse()?;
                return Ok(Self::Unknown);
            }
        }

        // VALUE or VALUE, "display"
        let value: LitInt = input.parse()?;
        let display = if input.peek(syn::Token![,]) {
            let _: syn::Token![,] = input.parse()?;
            Some(input.parse::<LitStr>()?)
        } else {
            None
        };
        Ok(Self::Value { value, display })
    }
}

fn parse_variant_attr(attrs: &[syn::Attribute]) -> syn::Result<Option<ParsedVariantAttr>> {
    for attr in attrs {
        if attr.path().is_ident("wire_enum") {
            let parsed: ParsedVariantAttr = attr.parse_args()?;
            return Ok(Some(parsed));
        }
    }
    Ok(None)
}

// ── Main derive ────────────────────────────────────────────────────────────

pub fn derive(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;

    let data = match &input.data {
        syn::Data::Enum(e) => e,
        _ => return Err(syn::Error::new_spanned(input, "WireEnum can only be derived for enums")),
    };

    let wire_type = WireType::from_attrs(&input.attrs)?;

    let mut known: Vec<KnownVariant> = Vec::new();
    let mut unknown_ident: Option<syn::Ident> = None;

    for variant in &data.variants {
        match parse_variant_attr(&variant.attrs)? {
            Some(ParsedVariantAttr::Value { value, display }) => {
                if !matches!(variant.fields, Fields::Unit) {
                    return Err(syn::Error::new_spanned(
                        variant,
                        "wire_enum value variants must be unit variants",
                    ));
                }
                known.push(KnownVariant { ident: variant.ident.clone(), value, display });
            }
            Some(ParsedVariantAttr::Unknown) => {
                // Validate: must be Variant(T) where T matches repr type.
                match &variant.fields {
                    Fields::Unnamed(f) if f.unnamed.len() == 1 => {
                        let ty = &f.unnamed.first().expect("len checked").ty;
                        let expected = wire_type.type_token().to_string();
                        let actual = quote!(#ty).to_string();
                        if actual != expected {
                            return Err(syn::Error::new_spanned(
                                ty,
                                format!(
                                    "#[wire_enum(unknown)] variant field must be {expected} (matching #[repr({expected})])"
                                ),
                            ));
                        }
                    }
                    _ => {
                        return Err(syn::Error::new_spanned(
                            variant,
                            "#[wire_enum(unknown)] variant must have exactly one unnamed field matching the repr type",
                        ));
                    }
                }
                if unknown_ident.is_some() {
                    return Err(syn::Error::new_spanned(
                        variant,
                        "only one #[wire_enum(unknown)] variant is allowed",
                    ));
                }
                unknown_ident = Some(variant.ident.clone());
            }
            None => {
                return Err(syn::Error::new_spanned(
                    variant,
                    "all WireEnum variants must have a #[wire_enum(...)] attribute",
                ));
            }
        }
    }

    let from_wire = gen_from_wire(name, &known, &unknown_ident, wire_type);
    let to_wire   = gen_to_wire(name, &known, &unknown_ident, wire_type);
    let display   = gen_display(name, &known, &unknown_ident, wire_type);
    let decode    = gen_decode(name, unknown_ident.is_some(), wire_type);
    let encode    = gen_encode(name, wire_type);

    Ok(quote! {
        #from_wire
        #to_wire
        #display
        #decode
        #encode
    })
}

// ── Code generators ────────────────────────────────────────────────────────

fn gen_from_wire(
    name: &syn::Ident,
    known: &[KnownVariant],
    unknown_ident: &Option<syn::Ident>,
    wire_type: WireType,
) -> TokenStream {
    let has_unknown = unknown_ident.is_some();
    let ty = wire_type.type_token();

    let match_arms: Vec<_> = known.iter().map(|v| {
        let val = &v.value;
        let ident = &v.ident;
        if has_unknown {
            quote! { #val => Self::#ident, }
        } else {
            quote! { #val => ::core::result::Result::Ok(Self::#ident), }
        }
    }).collect();

    let fallback = if let Some(unk) = unknown_ident {
        quote! { other => Self::#unk(other), }
    } else {
        quote! {
            other => ::core::result::Result::Err(
                ::u_io::DecodeError::Other(
                    ::std::format!("unknown {} value: 0x{:X}", ::core::stringify!(#name), other),
                )
            ),
        }
    };

    let ret_ty = if has_unknown {
        quote! { Self }
    } else {
        quote! { ::core::result::Result<Self, ::u_io::DecodeError> }
    };

    quote! {
        impl #name {
            pub fn from_wire(v: #ty) -> #ret_ty {
                match v {
                    #(#match_arms)*
                    #fallback
                }
            }
        }
    }
}

fn gen_to_wire(
    name: &syn::Ident,
    known: &[KnownVariant],
    unknown_ident: &Option<syn::Ident>,
    wire_type: WireType,
) -> TokenStream {
    let ty = wire_type.type_token();

    let match_arms: Vec<_> = known.iter().map(|v| {
        let val = &v.value;
        let ident = &v.ident;
        quote! { Self::#ident => #val, }
    }).collect();

    let fallback = if let Some(unk) = unknown_ident {
        quote! { Self::#unk(code) => *code, }
    } else {
        quote! {}
    };

    quote! {
        impl #name {
            pub fn to_wire(&self) -> #ty {
                match self {
                    #(#match_arms)*
                    #fallback
                }
            }
        }
    }
}

fn gen_display(
    name: &syn::Ident,
    known: &[KnownVariant],
    unknown_ident: &Option<syn::Ident>,
    wire_type: WireType,
) -> TokenStream {
    let fmt_spec = wire_type.fmt_spec();

    let match_arms: Vec<_> = known.iter().map(|v| {
        let ident = &v.ident;
        let display_ts = v.display_tokens();
        quote! { Self::#ident => #display_ts, }
    }).collect();

    let fallback = if let Some(unk) = unknown_ident {
        // e.g. write!(f, "unknown (0x{code:04X})")
        let fmt_str = format!("unknown (0x{{code:{fmt_spec}}})");
        quote! { Self::#unk(code) => ::core::write!(f, #fmt_str), }
    } else {
        quote! {}
    };

    quote! {
        impl ::core::fmt::Display for #name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match self {
                    #(#match_arms)*
                    #fallback
                }
            }
        }
    }
}

fn gen_decode(name: &syn::Ident, has_unknown: bool, wire_type: WireType) -> TokenStream {
    let read_method = wire_type.read_method();

    let conversion = if has_unknown {
        quote! { ::core::result::Result::Ok(Self::from_wire(v)) }
    } else {
        quote! { Self::from_wire(v) }
    };

    quote! {
        impl<__E: ::u_io::ByteOrder> ::u_io::Decode<__E> for #name {
            fn decode<__R: ::u_io::ReadPrimitives<__E>>(
                __reader: &mut __R,
            ) -> ::core::result::Result<Self, ::u_io::DecodeError> {
                let v = __reader.#read_method()?;
                #conversion
            }
        }
    }
}

fn gen_encode(name: &syn::Ident, wire_type: WireType) -> TokenStream {
    let put_method = wire_type.put_method();

    quote! {
        impl<__E: ::u_io::ByteOrder> ::u_io::Encode<__E> for #name {
            fn encode(&self, __writer: &mut ::u_io::BinaryWriter<__E>) {
                __writer.#put_method(self.to_wire());
            }
        }
    }
}

//! Code generation for `#[derive(IntoLuaTable)]`.
//!
//! Generates `impl mlua::IntoLua for T` that converts a struct or enum
//! into a Lua table via `raw_set`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DataEnum, DataStruct, DeriveInput, Fields};

use crate::codegen::extract_named_fields;
use crate::lua_attrs::{
    LuaContainerAttrs, LuaFieldAttrs, LuaVariantAttrs, resolve_variant_name,
};

/// Top-level entry point: dispatches to struct or enum codegen.
pub fn derive(input: &DeriveInput) -> syn::Result<TokenStream> {
    let container_attrs = LuaContainerAttrs::from_attrs(&input.attrs)?;

    match &input.data {
        Data::Struct(data) => derive_struct(input, data),
        Data::Enum(data) => derive_enum(input, data, &container_attrs),
        Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "IntoLuaTable cannot be derived for unions",
        )),
    }
}

// ── Struct codegen ────────────────────────────────────────────────────────

fn derive_struct(input: &DeriveInput, data: &DataStruct) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let fields = extract_named_fields(data)?;

    let mut set_stmts = Vec::new();

    for field in fields {
        let ident = field.ident.as_ref().unwrap();
        let attrs = LuaFieldAttrs::from_attrs(&field.attrs)?;

        if attrs.skip {
            continue;
        }

        let key = attrs.lua_key(&ident.to_string()).to_owned();

        // Build the value expression, applying `into_via` if present.
        let value_expr = if let Some(ref via) = attrs.into_via {
            quote! { #via(self.#ident) }
        } else {
            quote! { self.#ident }
        };

        let stmt = if attrs.skip_none {
            // Only set the key when the Option is Some.
            quote! {
                if let ::core::option::Option::Some(__v) = #value_expr {
                    __table.raw_set(#key, __v)?;
                }
            }
        } else {
            quote! {
                __table.raw_set(#key, #value_expr)?;
            }
        };

        set_stmts.push(stmt);
    }

    Ok(quote! {
        impl ::mlua::IntoLua for #name {
            fn into_lua(self, __lua: &::mlua::Lua) -> ::mlua::Result<::mlua::Value> {
                let __table = __lua.create_table()?;
                #(#set_stmts)*
                ::core::result::Result::Ok(::mlua::Value::Table(__table))
            }
        }
    })
}

// ── Enum codegen ──────────────────────────────────────────────────────────

fn derive_enum(
    input: &DeriveInput,
    data: &DataEnum,
    container: &LuaContainerAttrs,
) -> syn::Result<TokenStream> {
    let name = &input.ident;

    let tag_key = container.tag.as_deref().unwrap_or("type");

    let mut match_arms = Vec::new();

    for variant in &data.variants {
        let variant_ident = &variant.ident;
        let variant_attrs = LuaVariantAttrs::from_attrs(&variant.attrs)?;

        if variant_attrs.skip {
            match_arms.push(quote! {
                Self::#variant_ident { .. } => {
                    ::core::unreachable!(
                        concat!("IntoLuaTable: variant `", stringify!(#variant_ident), "` is marked #[lua(skip)]")
                    )
                }
            });
            continue;
        }

        let tag_value = resolve_variant_name(
            &variant_ident.to_string(),
            &variant_attrs,
            container.rename_all,
        );

        match &variant.fields {
            Fields::Named(fields_named) => {
                let arm = gen_named_variant_arm(
                    variant_ident,
                    &fields_named.named,
                    &tag_key,
                    &tag_value,
                )?;
                match_arms.push(arm);
            }
            Fields::Unnamed(fields_unnamed) => {
                if fields_unnamed.unnamed.len() == 1 {
                    // Single-field tuple variant: delegate to inner type's IntoLua.
                    match_arms.push(quote! {
                        Self::#variant_ident(__inner) => {
                            ::mlua::IntoLua::into_lua(__inner, __lua)
                        }
                    });
                } else {
                    return Err(syn::Error::new_spanned(
                        variant,
                        "IntoLuaTable: tuple variants with multiple fields are not supported; \
                         use a named-field variant instead",
                    ));
                }
            }
            Fields::Unit => {
                // Unit variant: only the tag, no extra fields.
                match_arms.push(quote! {
                    Self::#variant_ident => {
                        let __table = __lua.create_table()?;
                        __table.raw_set(#tag_key, #tag_value)?;
                        ::core::result::Result::Ok(::mlua::Value::Table(__table))
                    }
                });
            }
        }
    }

    Ok(quote! {
        impl ::mlua::IntoLua for #name {
            fn into_lua(self, __lua: &::mlua::Lua) -> ::mlua::Result<::mlua::Value> {
                match self {
                    #(#match_arms)*
                }
            }
        }
    })
}

/// Generate a match arm for a named-field enum variant.
fn gen_named_variant_arm(
    variant_ident: &syn::Ident,
    fields: &syn::punctuated::Punctuated<syn::Field, syn::Token![,]>,
    tag_key: &str,
    tag_value: &str,
) -> syn::Result<TokenStream> {
    let mut field_bindings = Vec::new();
    let mut set_stmts = Vec::new();

    for field in fields {
        let ident = field.ident.as_ref().unwrap();
        let attrs = LuaFieldAttrs::from_attrs(&field.attrs)?;

        if attrs.skip {
            field_bindings.push(quote! { #ident: _ });
            continue;
        }

        field_bindings.push(quote! { #ident });

        let key = attrs.lua_key(&ident.to_string()).to_owned();

        let value_expr = if let Some(ref via) = attrs.into_via {
            quote! { #via(#ident) }
        } else {
            quote! { #ident }
        };

        let stmt = if attrs.skip_none {
            quote! {
                if let ::core::option::Option::Some(__v) = #value_expr {
                    __table.raw_set(#key, __v)?;
                }
            }
        } else {
            quote! {
                __table.raw_set(#key, #value_expr)?;
            }
        };

        set_stmts.push(stmt);
    }

    Ok(quote! {
        Self::#variant_ident { #(#field_bindings),* } => {
            let __table = __lua.create_table()?;
            __table.raw_set(#tag_key, #tag_value)?;
            #(#set_stmts)*
            ::core::result::Result::Ok(::mlua::Value::Table(__table))
        }
    })
}

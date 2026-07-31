//! Code generation for `#[derive(FromLuaTable)]`.
//!
//! Generates `impl mlua::FromLua for T` that reads a struct's fields
//! from a Lua table.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DataStruct, DeriveInput};

use crate::codegen::extract_named_fields;
use crate::lua_attrs::LuaFieldAttrs;

/// Top-level entry point.
pub fn derive(input: &DeriveInput) -> syn::Result<TokenStream> {
    match &input.data {
        Data::Struct(data) => derive_struct(input, data),
        Data::Enum(_) => Err(syn::Error::new_spanned(
            input,
            "FromLuaTable can currently only be derived for structs",
        )),
        Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "FromLuaTable cannot be derived for unions",
        )),
    }
}

fn derive_struct(input: &DeriveInput, data: &DataStruct) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let name_str = name.to_string();
    let fields = extract_named_fields(data)?;

    let mut field_inits = Vec::new();
    let mut all_have_defaults = true;

    for field in fields {
        let ident = field.ident.as_ref().unwrap();
        let ty = &field.ty;
        let attrs = LuaFieldAttrs::from_attrs(&field.attrs)?;

        if attrs.skip {
            // Use Default::default() for skipped fields.
            field_inits.push(quote! {
                #ident: ::core::default::Default::default()
            });
            continue;
        }

        let key = attrs.lua_key(&ident.to_string()).to_owned();

        let has_default = attrs.default || attrs.default_value.is_some();
        if !has_default {
            all_have_defaults = false;
        }

        let init_expr = if attrs.skip {
            unreachable!() // handled above
        } else if let Some(ref from_via) = attrs.from_via {
            // Custom conversion function.
            if let Some(ref default_expr) = attrs.default_value {
                quote! {
                    match __table.get::<::mlua::Value>(#key)? {
                        ::mlua::Value::Nil => #default_expr,
                        __v => #from_via(::mlua::FromLua::from_lua(__v, __lua)?),
                    }
                }
            } else if attrs.default {
                quote! {
                    match __table.get::<::mlua::Value>(#key)? {
                        ::mlua::Value::Nil => ::core::default::Default::default(),
                        __v => #from_via(::mlua::FromLua::from_lua(__v, __lua)?),
                    }
                }
            } else {
                quote! {
                    #from_via(__table.get::<#ty>(#key)?)
                }
            }
        } else if let Some(ref default_expr) = attrs.default_value {
            // `#[lua(default = EXPR)]`
            quote! {
                __table.get::<#ty>(#key).unwrap_or(#default_expr)
            }
        } else if attrs.default {
            // `#[lua(default)]`
            quote! {
                __table.get::<#ty>(#key).unwrap_or_default()
            }
        } else {
            // Required field — propagate error.
            quote! {
                __table.get::<#ty>(#key)?
            }
        };

        field_inits.push(quote! { #ident: #init_expr });
    }

    // If all fields have defaults, also accept Nil (the whole table is optional).
    let nil_branch = if all_have_defaults {
        let default_inits: Vec<TokenStream> = fields
            .iter()
            .map(|field| {
                let ident = field.ident.as_ref().unwrap();
                let attrs = LuaFieldAttrs::from_attrs(&field.attrs).unwrap();

                if attrs.skip {
                    return quote! { #ident: ::core::default::Default::default() };
                }
                if let Some(ref default_expr) = attrs.default_value {
                    quote! { #ident: #default_expr }
                } else {
                    // attrs.default must be true (all_have_defaults is true and !skip)
                    quote! { #ident: ::core::default::Default::default() }
                }
            })
            .collect();

        quote! {
            ::mlua::Value::Nil => {
                return ::core::result::Result::Ok(Self {
                    #(#default_inits),*
                });
            }
        }
    } else {
        quote! {}
    };

    Ok(quote! {
        impl ::mlua::FromLua for #name {
            fn from_lua(
                __value: ::mlua::Value,
                __lua: &::mlua::Lua,
            ) -> ::mlua::Result<Self> {
                let __table = match __value {
                    ::mlua::Value::Table(__t) => __t,
                    #nil_branch
                    _ => {
                        return ::core::result::Result::Err(
                            ::mlua::Error::FromLuaConversionError {
                                from: __value.type_name(),
                                to: ::std::string::String::from(#name_str),
                                message: ::core::option::Option::Some(
                                    ::std::string::String::from("expected table")
                                ),
                            }
                        );
                    }
                };

                ::core::result::Result::Ok(Self {
                    #(#field_inits),*
                })
            }
        }
    })
}

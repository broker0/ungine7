//! Parsing of `#[lua(...)]` attributes for Lua table derive macros.
//!
//! Two-level attribute system:
//! - **Container-level** (`LuaContainerAttrs`): `#[lua(tag = "...", rename_all = "...")]`
//! - **Field/variant-level** (`LuaFieldAttrs`): `#[lua(rename = "...", skip, skip_none, ...)]`

use syn::{Attribute, Expr, LitStr, Path};

// ── Container-level #[lua(...)] ───────────────────────────────────────────

/// Naming convention for `rename_all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameAll {
    /// `PascalCase` variant names → `snake_case` Lua strings.
    SnakeCase,
}

/// Parsed container-level (struct/enum) `#[lua(...)]` attributes.
#[derive(Debug, Clone)]
pub struct LuaContainerAttrs {
    /// For enums: the key name used as the discriminant in the Lua table.
    /// e.g. `#[lua(tag = "type")]` → `table.raw_set("type", "variant_name")`.
    pub tag: Option<String>,

    /// For enums: automatic variant name conversion.
    /// e.g. `#[lua(rename_all = "snake_case")]`.
    pub rename_all: Option<RenameAll>,
}

impl LuaContainerAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> syn::Result<Self> {
        let mut tag = None;
        let mut rename_all = None;

        for attr in attrs {
            if !attr.path().is_ident("lua") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("tag") {
                    let value: LitStr = meta.value()?.parse()?;
                    tag = Some(value.value());
                    Ok(())
                } else if meta.path.is_ident("rename_all") {
                    let value: LitStr = meta.value()?.parse()?;
                    rename_all = match value.value().as_str() {
                        "snake_case" => Some(RenameAll::SnakeCase),
                        other => {
                            return Err(syn::Error::new_spanned(
                                &value,
                                format!(
                                    "unknown rename_all value: \"{other}\", expected \"snake_case\""
                                ),
                            ));
                        }
                    };
                    Ok(())
                } else {
                    Err(meta.error("unknown #[lua(...)] attribute on container"))
                }
            })?;
        }

        Ok(Self { tag, rename_all })
    }
}

// ── Field-level #[lua(...)] ───────────────────────────────────────────────

/// Parsed field-level `#[lua(...)]` attributes.
///
/// Used for both struct fields and enum variant fields.
#[derive(Debug, Clone, Default)]
pub struct LuaFieldAttrs {
    /// `#[lua(rename = "name")]` — use a different key in the Lua table.
    pub rename: Option<String>,

    /// `#[lua(skip)]` — do not include this field in the Lua table.
    /// For `FromLuaTable`: uses `Default::default()`.
    pub skip: bool,

    /// `#[lua(skip_none)]` — only set the key when the value is `Some`.
    /// Only valid on `Option<T>` fields. (IntoLua direction only.)
    pub skip_none: bool,

    /// `#[lua(into_via = "path")]` — convert the field value through `path(value)`
    /// before inserting into the Lua table.
    pub into_via: Option<Path>,

    /// `#[lua(from_via = "path")]` — convert the raw Lua value through `path(value)`
    /// when reading from the Lua table.
    pub from_via: Option<Path>,

    /// `#[lua(default)]` — use `Default::default()` when the key is absent (FromLua only).
    pub default: bool,

    /// `#[lua(default = EXPR)]` — use EXPR when the key is absent (FromLua only).
    pub default_value: Option<Expr>,
}

impl LuaFieldAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> syn::Result<Self> {
        let mut result = Self::default();

        for attr in attrs {
            if !attr.path().is_ident("lua") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename") {
                    let value: LitStr = meta.value()?.parse()?;
                    result.rename = Some(value.value());
                } else if meta.path.is_ident("skip") {
                    result.skip = true;
                } else if meta.path.is_ident("skip_none") {
                    result.skip_none = true;
                } else if meta.path.is_ident("into_via") {
                    let value: LitStr = meta.value()?.parse()?;
                    result.into_via = Some(value.parse()?);
                } else if meta.path.is_ident("from_via") {
                    let value: LitStr = meta.value()?.parse()?;
                    result.from_via = Some(value.parse()?);
                } else if meta.path.is_ident("default") {
                    // Peek: if there's a `=` after `default`, parse the expression.
                    // Otherwise it's a bare `default` keyword.
                    if meta.input.peek(syn::Token![=]) {
                        let value: Expr = meta.value()?.parse()?;
                        result.default_value = Some(value);
                    } else {
                        result.default = true;
                    }
                } else {
                    return Err(meta.error("unknown #[lua(...)] attribute on field"));
                }
                Ok(())
            })?;
        }

        // Validation: conflicting attributes.
        if result.skip {
            if result.rename.is_some()
                || result.skip_none
                || result.into_via.is_some()
                || result.from_via.is_some()
                || result.default
                || result.default_value.is_some()
            {
                return Err(syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "#[lua(skip)] conflicts with all other #[lua(...)] field attributes",
                ));
            }
        }

        if result.default && result.default_value.is_some() {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "#[lua(default)] and #[lua(default = EXPR)] are mutually exclusive",
            ));
        }

        Ok(result)
    }

    /// Resolve the Lua key name for a field.
    ///
    /// Returns `rename` if set, otherwise the Rust field identifier as a string.
    pub fn lua_key<'a>(&'a self, rust_name: &'a str) -> &'a str {
        self.rename.as_deref().unwrap_or(rust_name)
    }
}

// ── Variant-level #[lua(...)] ─────────────────────────────────────────────

/// Parsed variant-level `#[lua(...)]` attributes (for enums).
#[derive(Debug, Clone, Default)]
pub struct LuaVariantAttrs {
    /// `#[lua(rename = "name")]` — explicit tag value for this variant.
    /// Overrides `rename_all` on the container.
    pub rename: Option<String>,

    /// `#[lua(skip)]` — this variant should not appear in Lua.
    /// Generates `unreachable!()` in the match arm.
    pub skip: bool,
}

impl LuaVariantAttrs {
    pub fn from_attrs(attrs: &[Attribute]) -> syn::Result<Self> {
        let mut result = Self::default();

        for attr in attrs {
            if !attr.path().is_ident("lua") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename") {
                    let value: LitStr = meta.value()?.parse()?;
                    result.rename = Some(value.value());
                } else if meta.path.is_ident("skip") {
                    result.skip = true;
                } else {
                    return Err(meta.error("unknown #[lua(...)] attribute on variant"));
                }
                Ok(())
            })?;
        }

        if result.skip && result.rename.is_some() {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "#[lua(skip)] and #[lua(rename = ...)] are mutually exclusive on a variant",
            ));
        }

        Ok(result)
    }
}

// ── rename_all helpers ────────────────────────────────────────────────────

/// Convert a PascalCase identifier to snake_case.
///
/// Examples: `EntityMoved` → `entity_moved`, `HPUpdated` → `hp_updated`.
pub fn pascal_to_snake_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    let chars: Vec<char> = s.chars().collect();

    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                let prev = chars[i - 1];
                let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());

                // Insert underscore before an uppercase letter when:
                //   - previous char was lowercase (normal boundary: "eM" → "e_m")
                //   - previous char was uppercase AND next is lowercase
                //     (end of acronym: "PU" in "HPUpdated" → "hp_u")
                if prev.is_lowercase() || (prev.is_uppercase() && next_lower) {
                    result.push('_');
                }
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }

    result
}

/// Apply `rename_all` to a variant name, respecting an explicit `rename` override.
pub fn resolve_variant_name(
    rust_name: &str,
    variant_attrs: &LuaVariantAttrs,
    rename_all: Option<RenameAll>,
) -> String {
    if let Some(ref explicit) = variant_attrs.rename {
        return explicit.clone();
    }
    match rename_all {
        Some(RenameAll::SnakeCase) => pascal_to_snake_case(rust_name),
        None => rust_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_to_snake() {
        assert_eq!(pascal_to_snake_case("EntityMoved"), "entity_moved");
        assert_eq!(pascal_to_snake_case("HPUpdated"), "hp_updated");
        assert_eq!(pascal_to_snake_case("MobileAppeared"), "mobile_appeared");
        assert_eq!(pascal_to_snake_case("GlobalLight"), "global_light");
        assert_eq!(pascal_to_snake_case("ClilocMessage"), "cliloc_message");
        assert_eq!(pascal_to_snake_case("X"), "x");
        assert_eq!(pascal_to_snake_case(""), "");
        assert_eq!(pascal_to_snake_case("DamageDealt"), "damage_dealt");
        assert_eq!(pascal_to_snake_case("GumpOpened"), "gump_opened");
    }
}

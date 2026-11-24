// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Validating options into the chains a creation passes on (spec §3.2).

use super::*;

pub(super) const OPTIONS: &[&str] = &["init", "equals", "key", "batches", "resilience", "disposed"];

/// Validated options, as token streams ready for the expansion.
pub(super) struct Resolved {
    pub(super) init: Option<Expr>,
    /// The `Shape` of the node: its `batches`, `resilience` and `disposed` options.
    pub(super) shape: TokenStream,
    /// `.equals(…)`, or nothing.
    pub(super) equals: TokenStream,
    pub(super) key: Option<KeyArg>,
}

pub(super) fn resolve_options(input: &EffectInput, form: &Form<'_>) -> syn::Result<Resolved> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut init: Option<Expr> = None;
    let mut shape: TokenStream = quote!(::lithia::__private::Shape::new());
    let mut equals: TokenStream = TokenStream::new();
    let mut key: Option<KeyArg> = None;
    let mut errors: Option<syn::Error> = None;
    for option in &input.options {
        let name: String = option.name.to_string();
        if !OPTIONS.contains(&name.as_str()) {
            combine(
                &mut errors,
                syn::Error::new_spanned(
                    &option.name,
                    format!("unknown option `{name}`; expected one of {}", OPTIONS.join(", ")),
                ),
            );
            continue;
        }
        if !seen.insert(name.clone()) {
            combine(&mut errors, syn::Error::new_spanned(&option.name, format!("option `{name}` is given twice")));
            continue;
        }
        let value: &Expr = &option.value;
        match name.as_str() {
            "init" => {
                if let Form::Source(_) = form {
                    combine(
                        &mut errors,
                        syn::Error::new_spanned(
                            &option.name,
                            "`init` applies to derived nodes and async source nodes; a source node starts with its value",
                        ),
                    );
                    continue;
                }
                init = Some(value.clone());
            }
            "equals" => {
                equals = quote!(.equals(#value));
            }
            "batches" | "resilience" | "disposed" => {
                if name != "disposed" && !matches!(form, Form::Derived(_)) {
                    combine(
                        &mut errors,
                        syn::Error::new_spanned(
                            &option.name,
                            format!("`{name}` applies to derived nodes only; a source node has no parents"),
                        ),
                    );
                    continue;
                }
                // The option's constants, and only its own, resolve inside its value.
                let setter: &Ident = &option.name;
                shape = quote!(#shape.#setter({
                    #[allow(unused_imports)]
                    use ::lithia::#setter::*;
                    #value
                }));
            }
            "key" => match KeyArg::parse(value) {
                Ok(parsed) => key = Some(parsed),
                Err(error) => combine(&mut errors, error),
            },
            _ => unreachable!(),
        }
    }
    match errors {
        Some(errors) => Err(errors),
        None => Ok(Resolved { init, shape, equals, key }),
    }
}

/// The options chains of a creation: `create` for a new node, `same` for a reclaimed one; a keyed
/// creation evaluates its shape once, in `bind`, before the lookup.
pub(super) struct Chains {
    pub(super) bind: TokenStream,
    pub(super) create: TokenStream,
    pub(super) same: TokenStream,
}

impl Chains {
    pub(super) fn of(resolved: &Resolved) -> Chains {
        let Resolved { shape, equals, key, .. }: &Resolved = resolved;
        match key {
            None => Chains {
                bind: TokenStream::new(),
                create: quote!(::lithia::__private::Options::new(#shape) #equals),
                same: TokenStream::new(),
            },
            Some(_) => {
                let same: TokenStream = quote!(::lithia::__private::Options::new(__lithia_shape) #equals);
                Chains {
                    bind: quote!(let __lithia_shape: ::lithia::__private::Shape = #shape;),
                    create: quote!(#same.keyed(__lithia_reclaim, __lithia_key, __lithia_inputs)),
                    same,
                }
            }
        }
    }
}

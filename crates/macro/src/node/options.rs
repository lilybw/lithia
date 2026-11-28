// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Validating options into the chains a creation passes on (spec §3.2).

use super::*;

pub(super) const OPTIONS: &[&str] = &["init", "equals", "no_equals", "key", "batches", "resilience", "disposed"];

/// Validated options, as token streams ready for the expansion.
pub(super) struct Resolved {
    pub(super) init: Option<Expr>,
    /// The `Shape` of the node: its `batches`, `resilience` and `disposed` options.
    pub(super) shape: TokenStream,
    /// `.equals(…)`, or nothing.
    pub(super) equals: TokenStream,
    /// What picks the comparator by type when neither `equals` nor `no_equals` is given (spec §6.4).
    pub(super) implicit: TokenStream,
    pub(super) key: Option<KeyArg>,
}

pub(super) fn resolve_options(input: &NodeInput, form: &Form<'_>) -> syn::Result<Resolved> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut init: Option<Expr> = None;
    let mut shape: TokenStream = quote!(::lithia::__private::Shape::new());
    let mut equals: TokenStream = TokenStream::new();
    let mut key: Option<KeyArg> = None;
    let mut implicit: TokenStream = quote!(|__lithia_p: ::lithia::__private::Implicit<_>| {
        #[allow(unused_imports)]
        use ::lithia::__private::{ImplicitViaEq as _, ImplicitViaNone as _};
        (&&__lithia_p).__lithia_implicit()
    });
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
        let value: &Expr = match (&option.value, name.as_str()) {
            (None, "no_equals") => {
                if seen.contains("equals") {
                    combine(
                        &mut errors,
                        syn::Error::new_spanned(&option.name, "`no_equals` and `equals` exclude each other"),
                    );
                }
                implicit = quote!(::lithia::__private::no_implicit);
                continue;
            }
            (Some(_), "no_equals") => {
                combine(&mut errors, syn::Error::new_spanned(&option.name, "`no_equals` takes no value"));
                continue;
            }
            (None, _) => {
                combine(
                    &mut errors,
                    syn::Error::new_spanned(&option.name, format!("`{name}` needs a value: `{name} = …`")),
                );
                continue;
            }
            (Some(value), _) => value,
        };
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
                if seen.contains("no_equals") {
                    combine(
                        &mut errors,
                        syn::Error::new_spanned(&option.name, "`no_equals` and `equals` exclude each other"),
                    );
                }
                equals = quote!(.equals(#value));
                implicit = quote!(::lithia::__private::no_implicit);
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
        None => Ok(Resolved { init, shape, equals, implicit, key }),
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
        let Resolved { shape, equals, implicit, key, .. }: &Resolved = resolved;
        // Each chain ends a constructor's arguments: the options, then the comparator picked by type.
        match key {
            None => Chains {
                bind: TokenStream::new(),
                create: quote!(::lithia::__private::Options::new(#shape) #equals, #implicit),
                same: TokenStream::new(),
            },
            Some(_) => {
                let options: TokenStream = quote!(::lithia::__private::Options::new(__lithia_shape) #equals);
                Chains {
                    bind: quote!(let __lithia_shape: ::lithia::__private::Shape = #shape;),
                    create: quote!(#options.keyed(__lithia_reclaim, __lithia_key, __lithia_inputs), #implicit),
                    same: quote!(#options, #implicit),
                }
            }
        }
    }
}

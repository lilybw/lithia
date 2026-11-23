// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The pieces every expansion emits: its site, captures and their rebinding (spec §4.2).

use super::*;

/// How much of the macro input a site keeps, in characters.
const SITE_TOKENS: usize = 80;

/// The expansion's static `Site`; it keeps the start of the input only in builds with debug
/// assertions, so a release binary holds no body source.
pub(super) fn site(tokens: &str) -> TokenStream {
    let start: String = match tokens.char_indices().nth(SITE_TOKENS) {
        Some((end, _)) => format!("{}…", tokens[..end].trim_end()),
        None => tokens.to_string(),
    };
    quote! {
        fn __lithia_scope() {}
        fn __lithia_path() -> &'static str {
            ::lithia::__private::fn_path(::core::any::type_name_of_val(&__lithia_scope))
        }
        static __LITHIA_SITE: ::lithia::Site = ::lithia::Site::__new(
            ::core::file!(),
            ::core::line!(),
            ::core::column!(),
            ::core::module_path!(),
            __lithia_path,
            if ::core::cfg!(debug_assertions) { #start } else { "" },
        );
    }
}

/// Binds `ident` to its capture: an internal clone, or, in a `move` body, the moved value
/// flagged internal. `dependency` names are handles, so their whole capture can be read-only.
pub(super) fn capture_take(ident: &Ident, class: &Class, moved: bool, dependency: bool) -> TokenStream {
    // A `move` body is analysed by name only.
    let read_only: bool = *class == Class::ReadOnly;
    if let (Class::Paths { all, written }, false, false) = (class, moved, dependency) {
        return path_take(&quote!(#ident), ident, all, written);
    }
    if let (Class::Paths { all, written }, true, false) = (class, moved, dependency) {
        return moved_paths_take(ident, all, written);
    }
    if moved {
        // The body moves the value as written; a handle is flagged internal in place, and a
        // dependency through `Readable`, so generic code is flagged too.
        return match (dependency, read_only) {
            (true, _) => quote_spanned!(ident.span()=> ::lithia::__private::move_dep(&#ident, #read_only);),
            (false, false) => quote_spanned!(ident.span()=> (&#ident).__lithia_move();),
            (false, true) => quote_spanned!(ident.span()=> (&#ident).__lithia_move_ro();),
        };
    }
    // Any name other than a dependency is read-only only when its type is a handle, which
    // method dispatch decides.
    let value: TokenStream = match (read_only, dependency) {
        (true, true) => quote_spanned! {ident.span()=> {
            let __lithia_ro: ::lithia::__private::ReadOnly = __lithia_cap.read_only(true);
            (#ident).__lithia_capture()
        }},
        (true, false) => quote_spanned!(ident.span()=> (#ident).__lithia_capture_ro()),
        (false, _) => quote_spanned!(ident.span()=> (#ident).__lithia_capture()),
    };
    quote_spanned! {ident.span()=>
        #[allow(unused_variables, clippy::shadow_reuse, clippy::shadow_same, clippy::shadow_unrelated)]
        let #ident = #value;
    }
}

/// Clones a value used only through field paths: when every path is a handle, the clone
/// records no edges and each path that may write records its own.
pub(super) fn path_take(base: &TokenStream, binding: &Ident, all: &[FieldPath], written: &[FieldPath]) -> TokenStream {
    let checks: Vec<TokenStream> =
        all.iter().map(|fields: &FieldPath| quote!(&& (#base #(.#fields)*).__lithia_is_handle())).collect();
    let edges: TokenStream = if written.is_empty() {
        TokenStream::new()
    } else {
        let edges: Vec<TokenStream> =
            written.iter().map(|fields: &FieldPath| quote!((#base #(.#fields)*).__lithia_field();)).collect();
        quote!(if __lithia_paths { #(#edges)* })
    };
    quote_spanned! {binding.span()=>
        let __lithia_paths: bool = true #(#checks)*;
        #edges
        #[allow(unused_variables, clippy::shadow_reuse, clippy::shadow_same, clippy::shadow_unrelated)]
        let #binding = {
            let __lithia_ro: ::lithia::__private::ReadOnly = __lithia_cap.read_only(__lithia_paths);
            (#base).__lithia_capture()
        };
    }
}

/// Flags the handles of a moved value used only through field paths: when every path is a
/// handle, each is flagged internal in place, read-only or not by its own uses.
pub(super) fn moved_paths_take(ident: &Ident, all: &[FieldPath], written: &[FieldPath]) -> TokenStream {
    let checks: Vec<TokenStream> =
        all.iter().map(|fields: &FieldPath| quote!(&& (#ident #(.#fields)*).__lithia_is_handle())).collect();
    let flags: Vec<TokenStream> = all
        .iter()
        .map(|fields: &FieldPath| {
            if written.contains(fields) {
                quote!((&#ident #(.#fields)*).__lithia_move();)
            } else {
                quote!((&#ident #(.#fields)*).__lithia_move_ro();)
            }
        })
        .collect();
    quote_spanned! {ident.span()=>
        if true #(#checks)* {
            #(#flags)*
        } else {
            (&#ident).__lithia_move();
        }
    }
}

pub(super) fn self_clone(class: &Class) -> TokenStream {
    let binding: Ident = format_ident!("__self");
    if let Class::Paths { all, written } = class {
        return path_take(&quote!(self), &binding, all, written);
    }
    quote! {
        #[allow(unused_variables, clippy::shadow_reuse, clippy::shadow_same, clippy::shadow_unrelated)]
        let __self = (self).__lithia_capture();
    }
}

/// Opens the capture session an expansion's captures are recorded in, with the traits the
/// captures dispatch on in scope.
pub(super) fn session_begin() -> TokenStream {
    quote! {
        #[allow(unused_imports)]
        use ::lithia::__private::{CaptureAny as _, FieldAny as _, MoveAny as _};
        let __lithia_cap = ::lithia::__private::Capturing::begin();
    }
}

/// Closes the capture session; what it recorded goes to node creation.
pub(super) fn session_end() -> TokenStream {
    quote!(let __lithia_captured: ::lithia::__private::Captured = __lithia_cap.finish();)
}

/// Rebinds each capture by reference at the top of a body.
pub(super) fn by_reference(names: &[Ident]) -> TokenStream {
    quote! {
        #(
            #[allow(unused_variables, clippy::shadow_reuse, clippy::shadow_same, clippy::shadow_unrelated)]
            let #names = &#names;
        )*
    }
}

/// What a body captures: each name with its class, and `self`'s class if it uses `self`.
pub(super) struct Analysis {
    pub(super) names: Vec<Ident>,
    pub(super) classes: Vec<Class>,
    pub(super) self_class: Option<Class>,
}

pub(super) fn analysed(captures: Captures) -> Analysis {
    let classes: Vec<Class> = captures.found.iter().map(|ident: &Ident| captures.class(&ident.to_string())).collect();
    let self_class: Option<Class> = captures.uses_self.then(|| captures.class("self"));
    Analysis { names: captures.found, classes, self_class }
}

pub(super) fn analyse(body: &Expr) -> Analysis {
    let mut captures: Captures = Captures::new();
    captures.expr(body);
    analysed(captures)
}

pub(super) fn analyse_block(block: &syn::Block) -> Analysis {
    let mut captures: Captures = Captures::new();
    captures.block(block);
    analysed(captures)
}

/// Binds every capture by reference from the shared `Arc<Captures>` inside a run's future.
pub(super) fn unpack(names: &[Ident]) -> TokenStream {
    if names.is_empty() {
        return quote!(let _ = &__lithia_caps;);
    }
    quote! {
        #[allow(unused_variables, clippy::shadow_reuse, clippy::shadow_same, clippy::shadow_unrelated)]
        let (#(#names,)*) = &*__lithia_caps;
    }
}

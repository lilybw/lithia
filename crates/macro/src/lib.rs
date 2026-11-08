//! Procedural macros of lithia: `effect!`, `#[lithia::main]` and `#[lithia::test]`.
//! Use them through the `lithia` crate, whose paths their expansions name.

use proc_macro::TokenStream;

mod attrs;
mod captures;
mod effect;

/// Creates a node: a closure with parameters makes a derived node, anything else a source node.
/// `let doubled: DerivedNode<u32> = effect!(|count| *count.get() * 2);`
#[proc_macro]
pub fn effect(input: TokenStream) -> TokenStream {
    match syn::parse::<effect::EffectInput>(input).and_then(effect::expand) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Runs `async fn main` as a lithia task, so its polls form batches and `flush()` is scoped to it.
/// `#[lithia::main] async fn main() { let n: SourceNode<u32> = effect!(0); }`
#[proc_macro_attribute]
pub fn main(args: TokenStream, item: TokenStream) -> TokenStream {
    match attrs::main(args.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Runs an async test as a lithia task on a multi-threaded runtime by default.
/// `#[lithia::test] async fn counts() { lithia::flush().await; }`
#[proc_macro_attribute]
pub fn test(args: TokenStream, item: TokenStream) -> TokenStream {
    match attrs::test(args.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

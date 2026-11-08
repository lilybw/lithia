// Case AH: #[must_use] fires through the macro.
#![deny(unused_must_use)]
use lithia::{effect, SourceNode};

#[lithia::main]
async fn main() {
    let count: SourceNode<u32> = effect!(0);
    effect!(|count| println!("{}", count.get())); // error: unused `DerivedNode` that must be used
}

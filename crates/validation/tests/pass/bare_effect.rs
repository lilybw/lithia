// Case AH: a bare `effect!(…);` statement compiles without warnings.
#![deny(warnings)]
use lithia::{effect, SourceNode};

#[lithia::main]
async fn main() {
    let count: SourceNode<u32> = effect!(0);
    effect!(|count| println!("{}", count.get()));
    effect!(async |count| println!("{}", count.get()));
    effect!(async |count| loop {
        tokio::task::yield_now().await;
        let _ = count.get();
    });
    lithia::flush().await;
}

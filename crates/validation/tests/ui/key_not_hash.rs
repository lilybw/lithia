use lithia::{effect, SourceNode};

#[lithia::main]
async fn main() {
    let _n: SourceNode<u32> = effect!(1, key = 0.5f64);
}

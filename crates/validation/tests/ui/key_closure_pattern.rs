use lithia::{effect, SourceNode};

#[lithia::main]
async fn main() {
    let pair: (u32, u32) = (1, 2);
    let _n: SourceNode<u32> = effect!(1, key = |(a, _b)| *a);
    let _ = pair;
}

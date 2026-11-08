use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let pair: SourceNode<(i32, i32)> = effect!((1, 2));
    let _sum: DerivedNode<i32> = effect!(|(a, b)| a + b);
    let _ = pair;
}

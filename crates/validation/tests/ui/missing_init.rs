use lithia::{effect, DerivedNode, SourceNode};

struct NoDefault(u32);

fn main() {
    let n: SourceNode<u32> = effect!(1);
    let _wrapped: DerivedNode<NoDefault> = effect!(|n| NoDefault(*n.get()));
}

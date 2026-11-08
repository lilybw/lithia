use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let n: SourceNode<u32> = effect!(3);
    let _tick: DerivedNode<u32> = effect!(|n| {
        let inner = || {
            yield *n.get();
        };
        inner();
    });
}

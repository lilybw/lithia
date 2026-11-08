use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let n: SourceNode<u32> = effect!(3);
    let _tick: DerivedNode<u32> = effect!(async |n| {
        let inner = async {
            yield *n.get();
        };
        inner.await;
    });
}

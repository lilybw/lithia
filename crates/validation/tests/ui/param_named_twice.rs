use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let a: SourceNode<i32> = effect!(1);
    let _twice: DerivedNode<i32> = effect!(|a, a| *a.get());
}

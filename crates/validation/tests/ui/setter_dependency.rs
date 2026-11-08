use lithia::{effect, DerivedNode, Setter, SourceNode};

fn main() {
    let s: SourceNode<i32> = effect!(1);
    let writer: Setter<i32> = s.setter();
    let _child: DerivedNode<i32> = effect!(|writer| 0);
}

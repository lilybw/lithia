// An async source block sees its captures by reference: moving a handle out takes a clone.
use lithia::{effect, SourceNode};

fn main() {
    let n: SourceNode<u32> = effect!(1);
    let _block: SourceNode<()> = effect!(async {
        let _owned: SourceNode<u32> = n; // error: expected `SourceNode<u32>`, found `&SourceNode<u32>`
    });
}

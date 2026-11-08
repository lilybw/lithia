use std::rc::Rc;

use lithia::effect;

fn main() {
    let _ = effect!(Rc::new(1));
}

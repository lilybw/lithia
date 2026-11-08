use lithia::effect;

fn main() {
    let x: i32 = 1;
    let _ = effect!(|| x);
}

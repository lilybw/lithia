use lithia::effect;

fn main() {
    let _ = effect!(0, fragility = FRAGILE);
    let _ = effect!(async { 1 }, fragility = SEMI_FRAGILE);
}

// Every form of `effect!` takes `key`, as an expression or a closure naming variables in scope.
#![deny(warnings)]
use lithia::{effect, DerivedNode, SourceNode};

#[derive(Clone, PartialEq)]
struct Row {
    id: u32,
    group: u8,
    title: String,
}

#[lithia::main]
async fn main() {
    let rows: SourceNode<Vec<Row>> = effect!(Vec::new());
    let tick: SourceNode<u32> = effect!(0);
    let user: (u8, u8) = (1, 2);
    let lists: DerivedNode<Vec<(SourceNode<String>, SourceNode<String>, DerivedNode<usize>, DerivedNode<u32>, DerivedNode<u32>)>> =
        effect!(|rows| {
            rows.get()
                .iter()
                .map(|row: &Row| {
                    (
                        effect!(row.title.clone(), key = row.id),
                        effect!(async { row.title.clone() }, key = |row, user| (row.id, row.group, user.0)),
                        effect!(|tick| row.title.len() + *tick.get() as usize, key = row.id),
                        effect!(async |tick| *tick.get() + row.id, key = (row.id, row.group)),
                        effect!(|tick| {
                            yield *tick.get();
                        }, key = row.id),
                    )
                })
                .collect::<Vec<_>>()
        });
    lithia::flush().await;
    let _ = lists.get();
}

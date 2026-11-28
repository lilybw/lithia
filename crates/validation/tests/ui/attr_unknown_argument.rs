// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

#[lithia::test(bogus = 1, worker_threads = 0)]
async fn counts() {}

fn main() {
    let _ = counts;
}

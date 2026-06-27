//! v0.10 FS server (`workload=fs`): serves an **empty** [`RamFs`] over the FS
//! endpoint. The receive loop lives in [`fs::serve`]; this binary just supplies
//! the concrete (empty) filesystem. The seeded variant is `fs-server-seeded`.

#![no_std]
#![no_main]

use ramfs::RamFs;
use snitchos_user::entry;

#[entry]
fn main() {
    fs::serve(RamFs::new())
}

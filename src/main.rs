use std::env;
use std::os::raw::c_char;
use std::os::raw::c_int;

mod sqlite_wal;
mod consensus;

use consensus::WALStore;

fn main() {
    let argv: Vec<_> = env::args().collect();
    let args: Vec<_> = argv.into_iter().map(|arg| arg.as_ptr()).collect();

    // XXX: Hard coded node id
    let store = WALStore::new(1);
    sqlite_wal::register_wal();
    unsafe {
        rust_wal::shell_main(args.len() as c_int, args.as_ptr() as *mut *mut c_char);
    }
}

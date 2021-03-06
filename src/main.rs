use rust_wal::shell_main;

use std::env;
use std::os::raw::c_int;
use std::os::raw::c_char;

fn main() {
    let argv: Vec<_> = env::args().collect();
    let args:Vec<_> = argv.into_iter().map(|arg| { arg.as_ptr() } ).collect();
    unsafe {
        shell_main(args.len() as c_int, args.as_ptr() as *mut *mut c_char);
    }
}

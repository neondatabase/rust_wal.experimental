use std::os::raw::c_void;

use rust_wal;

extern "C" fn begin_tx(_wal: *mut rust_wal::sqlite3_wal_replication, _arg: *mut c_void) -> i32 {
    println!("Got tx begin");
    return 0;
}

extern "C" fn abort_tx(_wal: *mut rust_wal::sqlite3_wal_replication, _arg: *mut c_void) -> i32 {
    println!("Got tx abort");
    return 0;
}

// int (*xFrames)(sqlite3_wal_replication*, void *pArg, int szPage, int nFrame,
//                sqlite3_wal_replication_frame *aFrame, unsigned nTruncate, int isCommit);
extern "C" fn frames(
    _wal: *mut rust_wal::sqlite3_wal_replication,
    _arg: *mut c_void,
    _sz_page: i32,
    _num_frames: i32,
    _frame_array: *mut rust_wal::sqlite3_wal_replication_frame,
    _ntrunc: u32,
    _is_commit: i32,
) -> i32 {
    println!("Got frames");
    return 0;
}

extern "C" fn undo_tx(_wal: *mut rust_wal::sqlite3_wal_replication, _arg: *mut c_void) -> i32 {
    println!("Got undo tx");
    return 0;
}

extern "C" fn end_tx(_wal: *mut rust_wal::sqlite3_wal_replication, _arg: *mut c_void) -> i32 {
    println!("Got end tx");
    return 0;
}

pub fn register_wal() {
    unsafe {
        let mut wal_replication: *mut rust_wal::sqlite3_wal_replication = rust_wal::get_wal_replication();
        (*wal_replication).xBegin = Some(begin_tx);
        (*wal_replication).xAbort = Some(abort_tx);
        (*wal_replication).xFrames = Some(frames);
        (*wal_replication).xUndo = Some(undo_tx);
        (*wal_replication).xEnd = Some(end_tx);
        rust_wal::sqlite3_wal_replication_register(wal_replication, 1);
    }
}

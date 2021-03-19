use std::os::raw::c_char;
use std::os::raw::c_int;
use std::sync::Arc;

use structopt::clap::arg_enum;
use structopt::StructOpt;

mod consensus;
mod sqlite_wal;

use async_raft::Config;
use consensus::{RaftRouter, WALRaft, WALStore};

arg_enum! {
    #[derive(Debug)]
    enum Role {
        Leader,
        Follower,
        Learner,
    }
}

#[derive(StructOpt, Debug)]
#[structopt(name = "rust_wal")]
struct Options {
    // normal comments are just comments
    /// doc comments get turned into help
    #[structopt(short = "i", long = "id", default_value = "1")]
    id: u64,

    #[structopt(short = "r", long = "role", possible_values = &Role::variants(), case_insensitive = true, default_value = "follower")]
    role: Role,

    // The number of occurrences of the `v/verbose` flag
    /// Verbose mode (-v, -vv, -vvv, etc.)
    #[structopt(short = "v", long = "verbose", parse(from_occurrences))]
    verbose: u8,

    #[structopt(subcommand)]
    sub: Option<Subcommands>,
}

#[derive(Debug, PartialEq, StructOpt)]
enum Subcommands {
    // `external_subcommand` tells structopt to put
    // all the extra arguments into this Vec
    #[structopt(external_subcommand)]
    Other(Vec<String>),
}

#[tokio::main]
pub async fn main() {
    let options = Options::from_args();
    println!("{} starting up", options.id);

    let sub_args: Vec<_> = match options.sub {
        Some(Subcommands::Other(argv)) => argv.into_iter().map(|arg| arg.as_ptr()).collect(),
        None => vec![],
    };

    let arg0 = String::from("mycommand");
    let args = [vec![arg0.as_ptr()], sub_args].concat();

    let config = Arc::new(
        Config::build("zenith".into())
            .validate()
            .expect("failed to build Raft config"),
    );
    let network = Arc::new(RaftRouter::new(config.clone()));
    let store = Arc::new(WALStore::new(options.id));
    let raft = WALRaft::new(options.id, config, network, store);

    sqlite_wal::register_wal();
    unsafe {
        rust_wal::shell_main(args.len() as c_int, args.as_ptr() as *mut *mut c_char);
    }
}

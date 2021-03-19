#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

extern crate serde;

#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate serde_big_array;

pub mod consensus;

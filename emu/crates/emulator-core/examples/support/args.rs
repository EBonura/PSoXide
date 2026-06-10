#![allow(dead_code)]

use std::path::PathBuf;

pub fn take_string(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{flag} requires a value"))
}

pub fn take_path(args: &mut impl Iterator<Item = String>, flag: &str) -> PathBuf {
    PathBuf::from(take_string(args, flag))
}

pub fn take_u64(args: &mut impl Iterator<Item = String>, flag: &str) -> u64 {
    take_string(args, flag)
        .parse()
        .unwrap_or_else(|_| panic!("{flag} requires an integer"))
}

pub fn take_usize(args: &mut impl Iterator<Item = String>, flag: &str) -> usize {
    take_string(args, flag)
        .parse()
        .unwrap_or_else(|_| panic!("{flag} requires an integer"))
}

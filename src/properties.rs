//! Property tests for the pure parsers. Every runner uses a fixed seed, so a
//! failure reproduces on any machine.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};

pub(crate) const SEED: u64 = 0x5EED_D2E6;

pub(crate) fn config(cases: u32) -> Config {
    Config {
        cases,
        rng_seed: RngSeed::Fixed(SEED),
        failure_persistence: None,
        ..Config::default()
    }
}

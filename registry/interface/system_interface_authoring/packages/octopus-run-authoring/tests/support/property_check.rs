//! Runs one property over generated cases.
//!
//! The cases come from a fixed seed, so a failure names the same minimal case
//! on every machine. `PROPTEST_RNG_SEED` and `PROPTEST_CASES` widen the search.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestRunner};

const SEED: u64 = 0x0C70_9005;

pub(crate) fn check<S: Strategy>(
    cases: u32,
    strategy: S,
    property: impl Fn(S::Value) -> Result<(), TestCaseError>,
) {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = cases;
    }
    if config.rng_seed == RngSeed::Random {
        config.rng_seed = RngSeed::Fixed(SEED);
    }
    config.failure_persistence = None;
    config.max_shrink_iters = 256;
    let seed = config.rng_seed;
    if let Err(failure) = TestRunner::new(config).run(&strategy, property) {
        panic!("{failure}\nseed {seed}; set PROPTEST_RNG_SEED to search from another one");
    }
}

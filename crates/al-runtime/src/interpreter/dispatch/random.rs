//! RANDOM and RANDOMIZE, on a seeded linear congruential generator.
//!
//! The seed lives on the dispatch context, so a run with the default seed
//! produces the same sequence every time and a test can set its own.

use crate::interpreter::eval_error;
use crate::interpreter::scope::Eval;
use crate::interpreter::value::Value;

use super::{DispatchCtx, DEFAULT_RANDOM_SEED};

/// Advance the deterministic LCG one step and return 31 usable state bits.
/// LCG multiplier/increment (Knuth's MMIX constants). A 64-bit state needs a
/// 64-bit multiplier: the classic 32-bit `214_013` left the high half of the
/// state near-zero for the first several steps.
const LCG_MULTIPLIER: u64 = 6_364_136_223_846_793_005;

const LCG_INCREMENT: u64 = 1_442_695_040_888_963_407;

/// Advance the LCG and return 31 bits taken from the **top** of the state.
///
/// A power-of-two-modulus LCG has notoriously short cycles in its low-order
/// bits (bit *k* repeats with period `2^(k+1)`). `Random(n)` reduces the raw
/// value modulo `n`, which reads exactly those low bits — so a small `n` used
/// to ride a very short cycle (`Random(2)` alternating, and so on). Taking the
/// high bits of the state gives every output bit the full period. The
/// generator stays fully deterministic: same seed, same sequence.
fn lcg_step(ctx: &mut DispatchCtx) -> u64 {
    ctx.random_state = ctx
        .random_state
        .wrapping_mul(LCG_MULTIPLIER)
        .wrapping_add(LCG_INCREMENT);
    ctx.random_state >> 33
}

/// Combine two deterministic LCG steps into the next raw value in
/// `[0, 2^62)`, so `Random(n)` covers large `n` instead of being capped at
/// the 15 bits a single truncated step would provide.
fn next_random(ctx: &mut DispatchCtx) -> i64 {
    let hi = lcg_step(ctx);
    let lo = lcg_step(ctx);
    ((hi << 31) | lo) as i64
}

/// `Random(n)` — a deterministic pseudo-random Integer in `1..=n`.
pub(super) fn builtin_random(args: &[Value], ctx: &mut DispatchCtx) -> Eval {
    let n = match args {
        [Value::Integer(n)] => *n,
        _ => return eval_error("Random expects exactly 1 Integer argument"),
    };
    if n < 1 {
        return eval_error("Random: the maximum must be >= 1");
    }
    Eval::Normal(Value::Integer(next_random(ctx) % n + 1))
}

/// `Randomize([seed])` — re-seed the deterministic generator. Without a seed
/// the generator returns to the fixed default so runs stay reproducible.
pub(super) fn builtin_randomize(args: &[Value], ctx: &mut DispatchCtx) -> Eval {
    match args {
        [] => {
            ctx.random_state = DEFAULT_RANDOM_SEED;
            Eval::Normal(Value::Empty)
        }
        [Value::Integer(seed) | Value::BigInteger(seed)] => {
            ctx.random_state = *seed as u64;
            Eval::Normal(Value::Empty)
        }
        _ => eval_error("Randomize expects at most 1 Integer argument"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::interpreter::dispatch::routing::dispatch_call;
    use crate::interpreter::dispatch::test_support::{ctx, ok};

    #[test]
    fn random_is_deterministic_and_seedable() {
        let mut a = ctx();
        let mut b = ctx();
        let seq = |ctx: &mut DispatchCtx| {
            (0..5)
                .map(|_| {
                    ok(dispatch_call(
                        None,
                        "Random",
                        vec![Value::Integer(100)],
                        ctx,
                    ))
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(seq(&mut a), seq(&mut b), "fresh contexts share the seed");
        for value in seq(&mut a) {
            match value {
                Value::Integer(n) => assert!((1..=100).contains(&n)),
                other => panic!("Random must return Integer, got {other:?}"),
            }
        }
        // Re-seeding resets the sequence deterministically: after Randomize
        // (argument-less or with the default seed) both contexts replay the
        // exact sequence a fresh context produces.
        assert!(dispatch_call(None, "Randomize", vec![], &mut a)
            .into_value()
            .is_some());
        assert!(matches!(
            dispatch_call(
                None,
                "Randomize",
                vec![Value::Integer(DEFAULT_RANDOM_SEED as i64)],
                &mut b
            ),
            Eval::Normal(_)
        ));
        let fresh = seq(&mut ctx());
        assert_eq!(
            seq(&mut a),
            fresh,
            "argument-less Randomize resets to the fixed default sequence"
        );
        assert_eq!(
            seq(&mut b),
            fresh,
            "Randomize(default seed) resets to the fixed default sequence"
        );
    }

    #[test]
    fn random_covers_ranges_beyond_15_bits() {
        let mut ctx = ctx();
        let n = i32::MAX as i64;
        let mut max_seen = 0i64;
        for _ in 0..64 {
            match ok(dispatch_call(
                None,
                "Random",
                vec![Value::Integer(n)],
                &mut ctx,
            )) {
                Value::Integer(v) => {
                    assert!((1..=n).contains(&v));
                    max_seen = max_seen.max(v);
                }
                other => panic!("Random must return Integer, got {other:?}"),
            }
        }
        assert!(
            max_seen > 0x8000,
            "Random({n}) never exceeded 15 bits (max seen {max_seen}); the raw generator is too narrow"
        );
    }

    /// `Random(n)` reduces the raw value modulo `n`, so it reads the raw
    /// value's *low* bits. Taking those straight off the LCG state gave small
    /// `n` a pathologically short cycle (`Random(2)` strictly alternating).
    /// The output must be drawn from the state's high bits instead.
    #[test]
    fn small_random_does_not_ride_a_short_low_bit_cycle() {
        let mut ctx = ctx();
        let draws: Vec<i64> = (0..64)
            .map(|_| {
                match ok(dispatch_call(
                    None,
                    "Random",
                    vec![Value::Integer(2)],
                    &mut ctx,
                )) {
                    Value::Integer(v) => v,
                    other => panic!("Random must return Integer, got {other:?}"),
                }
            })
            .collect();
        assert!(
            draws.iter().all(|v| (1..=2).contains(v)),
            "Random(2) must stay in 1..=2, got {draws:?}"
        );
        assert!(
            draws.contains(&1) && draws.contains(&2),
            "Random(2) must produce both values, got {draws:?}"
        );
        let alternating = draws.windows(2).all(|pair| pair[0] != pair[1]);
        assert!(
            !alternating,
            "Random(2) alternated for 64 draws — the output still rides the LCG's low bit"
        );
    }
}

//! `JSON.SET`: text into a value.
//!
//! Three sets of rows, in this order:
//!
//! 1. RedisJSON's datasets, one case per fixture.
//! 2. `synth-<preset>`: every preset of [`crate::synth`], timed the same way.
//! 3. `synth-<preset>-warmstr`, `-keywarm-coldstr` and `-coldstr`: the same
//!    document parsed with the string cache in three controlled states.
//!
//! # How `ijson` interns, and what that means for these rows
//!
//! In the build RedisJSON uses (no `thread_safe` feature) strings live in
//! `ijson/src/unsafe_string.rs`:
//!
//! - Object keys and string values go through the same `IString::intern`.
//!   The empty string is a static, strings of up to 7 bytes are stored inline
//!   in the value word, and every longer string is looked up in, or inserted
//!   into, one process-wide `HashSet`. A miss allocates the string and inserts
//!   it; a hit bumps a reference count.
//! - The cache holds weak entries. When the last `IString` for a text drops,
//!   the entry is removed and the string freed. So once every value that
//!   contains a string is dropped, that string is no longer in the cache.
//!   The table itself keeps its capacity.
//! - `ijson::reinit_shared_string_cache()` replaces the table with an empty
//!   one. It is not used here: dropping the values already empties the cache,
//!   and a new table would add growth and rehashing to the timed parse.
//!
//! The harness keeps results alive until the clock stops, drops them, and
//! batches operations that are shorter than its minimum sample time. For the
//! rows of sets 1 and 2 that leaves the cache state dependent on document
//! size: when several parses share a sample, the first one interns and the
//! rest hit (mostly warm); when a parse is long enough to be a sample by
//! itself, the previous result was dropped before the clock started and every
//! timed parse begins with none of the document's strings interned (cold,
//! apart from repeats inside the document). Set 3 removes that ambiguity.
//!
//! # Cache state of the set 3 variants
//!
//! All three parse documents from one spec with salts (see `synth`): each key
//! and each string value carries a fixed-width tag, so all of them are long
//! enough to be interned, and documents with different salts have the same
//! structure and length but no tagged string in common. All three use
//! `bench_with_setup`, so the timed code is identical.
//!
//! - `-warmstr`: a parsed copy of the same text is held alive for the whole
//!   measurement. Every key and every string value of every timed parse is a
//!   cache hit.
//! - `-keywarm-coldstr`: a parsed copy with the same key salt and value salt 0
//!   is held alive, and each timed parse gets a text with a value salt taken
//!   from a counter that starts at 1 and never repeats. Keys hit; every
//!   distinct string value misses, because no document with that value salt
//!   has been parsed before and salt 0 is never handed out. This is a
//!   keyspace of similar records: shared field names, new contents.
//! - `-coldstr`: nothing is held alive, and each timed parse gets a text
//!   whose key salt and value salt both come from such a counter. No string
//!   of the document is in the cache when its parse starts: results of earlier
//!   samples have been dropped, and results still held within the current
//!   sample came from other salts. Within one document a string that occurs
//!   more than once still misses once and hits afterwards.
//!
//! A cold parse therefore pays lookup miss, allocation and insertion per
//! distinct string; it does not pay for growing the table.
//!
//! Texts for the cold variants are generated in the setup closure, outside
//! the timed region. `serde_json::Value` has no cache; the same documents are
//! run under `serde` for contrast, where the variants should not differ.

use crate::harness::Params;
use crate::{ijson_parse, serde_parse, synth, Ctx, Row, Work};

/// In a smoke run, synthetic documents larger than this are skipped.
const QUICK_MAX_BYTES: usize = 1_500_000;

/// Presets parsed in the three cache states: a small and a larger record list
/// (repeated keys, mostly distinct values), all-distinct long strings, and
/// long strings from a small pool.
const CACHE_STATE_PRESETS: &[&str] = &["records-1k", "records-100k", "str-unique", "str-repeated"];

pub fn run(ctx: &mut Ctx) {
    let fixtures = ctx.fixtures;
    for fx in fixtures {
        let row = |imp| Row { group: "parse", case: fx.name.clone(), imp, work: Work::Bytes(fx.text.len()) };
        ctx.bench(row("ijson"), || ijson_parse(&fx.text));
        ctx.bench(row("serde"), || serde_parse(&fx.text));
    }

    let quick = ctx.params.min_time_ns < Params::default().min_time_ns;
    for p in synth::presets() {
        if quick && p.approx_bytes > QUICK_MAX_BYTES {
            continue;
        }
        let case = format!("synth-{}", p.name);
        let row = |imp, bytes| Row { group: "parse", case: case.clone(), imp, work: Work::Bytes(bytes) };
        if !ctx.wants(&row("ijson", 0)) {
            continue;
        }
        let text = p.spec.generate_verified(&case);
        ctx.bench(row("ijson", text.len()), || ijson_parse(&text));
        ctx.bench(row("serde", text.len()), || serde_parse(&text));
    }

    for name in CACHE_STATE_PRESETS {
        cache_states(ctx, name);
    }
}

/// The warm, keys-warm and cold rows for one preset.
fn cache_states(ctx: &mut Ctx, name: &str) {
    let base = synth::preset(name).unwrap_or_else(|| panic!("no synth preset named {name}")).spec;
    let case = |variant: &str| format!("synth-{name}-{variant}");
    let variants = ["warmstr", "keywarm-coldstr", "coldstr"];
    let probe = |v: &str| Row { group: "parse", case: case(v), imp: "ijson", work: Work::Bytes(0) };
    if !variants.iter().any(|v| ctx.wants(&probe(v))) {
        return;
    }

    // Salt 0 is the pinned document; the counters below start at 1.
    let warm_text = base.salted(0, 0).generate_verified(&case("warmstr"));
    let bytes = warm_text.len();
    for (k, v) in [(0, 1), (1, 1)] {
        let other = base.salted(k, v).generate_verified(&case("coldstr"));
        assert_eq!(other.len(), bytes, "{name}: salted documents must have equal length");
        assert_ne!(other, warm_text, "{name}: salts must change the document");
    }
    let row = |variant: &str, imp| Row { group: "parse", case: case(variant), imp, work: Work::Bytes(bytes) };

    // Warm: the pinned copy keeps every string of `warm_text` interned.
    {
        let _pin = ijson_parse(&warm_text);
        ctx.bench_with_setup(row("warmstr", "ijson"), || (), |_| ijson_parse(&warm_text));
    }
    ctx.bench_with_setup(row("warmstr", "serde"), || (), |_| serde_parse(&warm_text));

    // Keys warm, values cold: the pin shares the key salt only.
    {
        let _pin = ijson_parse(&warm_text);
        let mut n = 0u64;
        let fresh = || {
            n += 1;
            base.salted(0, n).generate()
        };
        ctx.bench_with_setup(row("keywarm-coldstr", "ijson"), fresh, |t| ijson_parse(t));
    }
    {
        let mut n = 0u64;
        let fresh = || {
            n += 1;
            base.salted(0, n).generate()
        };
        ctx.bench_with_setup(row("keywarm-coldstr", "serde"), fresh, |t| serde_parse(t));
    }

    // Cold: nothing pinned, every text has keys and values no parse has seen.
    {
        let mut n = 0u64;
        let fresh = || {
            n += 1;
            base.salted(n, n).generate()
        };
        ctx.bench_with_setup(row("coldstr", "ijson"), fresh, |t| ijson_parse(t));
    }
    {
        let mut n = 0u64;
        let fresh = || {
            n += 1;
            base.salted(n, n).generate()
        };
        ctx.bench_with_setup(row("coldstr", "serde"), fresh, |t| serde_parse(t));
    }
}

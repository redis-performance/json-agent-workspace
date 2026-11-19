//! `JSON.GET`, default format: a value into text.
//!
//! Dataset rows come first, then one `synth-<preset>` case per preset of
//! [`crate::synth`]. Synthetic documents are generated and verified before
//! anything is timed. `Work::Bytes` is the length of the input text the value
//! was parsed from, as for the datasets.

use crate::harness::Params;
use crate::{ijson_parse, ijson_serialize, serde_parse, serde_serialize, synth, Ctx, Row, Work};

/// In a smoke run, synthetic documents larger than this are skipped.
const QUICK_MAX_BYTES: usize = 1_500_000;

pub fn run(ctx: &mut Ctx) {
    let fixtures = ctx.fixtures;
    for fx in fixtures {
        let row = |imp| Row { group: "serialize", case: fx.name.clone(), imp, work: Work::Bytes(fx.text.len()) };
        let iv = ijson_parse(&fx.text);
        let sv = serde_parse(&fx.text);
        ctx.bench(row("ijson"), || ijson_serialize(&iv));
        ctx.bench(row("serde"), || serde_serialize(&sv));
    }

    let quick = ctx.params.min_time_ns < Params::default().min_time_ns;
    for p in synth::presets() {
        if quick && p.approx_bytes > QUICK_MAX_BYTES {
            continue;
        }
        let case = format!("synth-{}", p.name);
        let row = |imp, bytes| Row { group: "serialize", case: case.clone(), imp, work: Work::Bytes(bytes) };
        if !ctx.wants(&row("ijson", 0)) {
            continue;
        }
        let text = p.spec.generate_verified(&case);
        let iv = ijson_parse(&text);
        let sv = serde_parse(&text);
        ctx.bench(row("ijson", text.len()), || ijson_serialize(&iv));
        ctx.bench(row("serde", text.len()), || serde_serialize(&sv));
    }
}

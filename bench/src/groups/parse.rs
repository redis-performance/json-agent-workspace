//! `JSON.SET`: text into a value.

use crate::{ijson_parse, serde_parse, Ctx, Row, Work};

pub fn run(ctx: &mut Ctx) {
    let fixtures = ctx.fixtures;
    for fx in fixtures {
        let row = |imp| Row { group: "parse", case: fx.name.clone(), imp, work: Work::Bytes(fx.text.len()) };
        ctx.bench(row("ijson"), || ijson_parse(&fx.text));
        ctx.bench(row("serde"), || serde_parse(&fx.text));
    }
}

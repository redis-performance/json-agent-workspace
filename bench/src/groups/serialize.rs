//! `JSON.GET`, default format: a value into text.

use crate::{ijson_parse, ijson_serialize, serde_parse, serde_serialize, Ctx, Row, Work};

pub fn run(ctx: &mut Ctx) {
    let fixtures = ctx.fixtures;
    for fx in fixtures {
        let row = |imp| Row { group: "serialize", case: fx.name.clone(), imp, work: Work::Bytes(fx.text.len()) };
        let iv = ijson_parse(&fx.text);
        let sv = serde_parse(&fx.text);
        ctx.bench(row("ijson"), || ijson_serialize(&iv));
        ctx.bench(row("serde"), || serde_serialize(&sv));
    }
}

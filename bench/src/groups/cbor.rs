//! RDB persistence: `ijson`'s CBOR codec as RedisJSON drives it, with the JSON
//! text round trip of the same document for contrast.
//!
//! RedisJSON (`redis_json/src/rdb_persistence.rs`, `redisjson.rs`) writes one
//! record per value: a marker byte, then CBOR. It never calls the zstd entry
//! points (`encode_compressed` / `decode_compressed`), so they are not here.
//! On load the depth-limit flag is `backward::legacy_rdb_depth_limit_enabled()`,
//! which is off by default: every decode here passes `limit_depth = false`.
//!
//! Rows are `<fixture>/<operation>` with `imp` naming the record form:
//!
//! - `encode` / `decode`, imp `cbor`: plain CBOR (marker 0). Encode is
//!   `ijson::encode_into` after a one-byte prefix, as in `encode_value`'s
//!   fallback; decode is `ijson::decode_with_depth_limit(bytes, false)`, as in
//!   `decode_for_load` (marker 0) and `redisjson.rs`, `load_rdb_value`
//!   (encoding version 4).
//! - `encode` / `decode`, imp `local`: the self-contained record of a value
//!   saved outside a stream (`encode_value`: DUMP, keyless saves, Flex, a save
//!   without a session). A fresh `StreamEncoder::new` with the default
//!   `AddressDictionary` and `table_budget_bytes = usize::MAX` (`local_config`)
//!   encodes the value; the result is kept (marker 3) only when it defined
//!   something and is smaller than plain (`pays`), otherwise the value is
//!   encoded again as plain CBOR. Decode follows `decode_for_load`: marker 0
//!   as above, marker 3 through a fresh `StreamDecoder::new().decode(..)`.
//!   The encoder or decoder is created and dropped inside the timed region,
//!   as it is in RedisJSON.
//! - `stream-encode-x16` / `stream-decode-x16`, imp `stream`: sixteen
//!   documents through one encoder or decoder, the keyed RDB save and load
//!   (`SaveSession::encode` with `begin_record` / `finish_record`, and
//!   `decode_stream_document`). The encoder is `StreamEncoder::new` with the
//!   default `AddressDictionary`; RedisJSON sets `table_budget_bytes` to 20%
//!   (`json-rdb-dictionary-max-memory-percent`) of the memory Redis uses when
//!   the save starts, and `STREAM_BUDGET` stands in for that. The sixteen
//!   documents are separate parses of the same text, so they share every
//!   interned string: after the first document all strings of eight bytes or
//!   more are references. This is the most a dictionary can save.
//! - `all-fixtures/stream-encode` / `stream-decode`, imp `stream`: every
//!   fixture once through one encoder or decoder, where little is shared
//!   between documents.
//! - `encode` / `decode`, imp `json`: `json_bench::ijson_serialize` and
//!   `json_bench::ijson_parse` of the same value.
//!
//! Output buffers: RedisJSON allocates a new `Vec<u8>` for every record
//! (`vec![MARKER_LOCAL]`, `vec![MARKER_INLINE; STREAM_HEADER_LEN]`) and does
//! not reuse one across values, so every encode here allocates its buffer
//! inside the timed region.
//!
//! Timed: the encode or decode calls above. Not timed: parsing the fixture,
//! building the records to decode, creating the stream encoder or decoder and
//! the vector that receives the sixteen results (both built by the setup of
//! `bench_with_setup`), and dropping the results and the stream state.
//!
//! `Work::Bytes` is the JSON text length of the fixture for encode (the input
//! document; times sixteen for the x16 stream), and the length of the encoded
//! records for decode (the bytes read; for `json` the length of the text that
//! is parsed).
//!
//! Every case is checked once before timing: each record is not empty and
//! decodes to a value equal to the one encoded.

use std::hint::black_box;

use ijson::{DocUse, IValue, StreamDecoder, StreamEncoder, StreamEncoderConfig};

use crate::{ijson_parse, ijson_serialize, Ctx, Row, Work};

const GROUP: &str = "cbor";

// Record framing of `rdb_persistence.rs`.
const MARKER_INLINE: u8 = 0;
const MARKER_STREAM: u8 = 1;
const MARKER_LOCAL: u8 = 3;
const STREAM_HEADER_LEN: usize = 5;

/// `legacy_rdb_depth_limit_enabled()` at its default.
const LIMIT_DEPTH: bool = false;

/// Documents per stream in the `x16` cases.
const STREAM_DOCS: usize = 16;

/// Stands in for 20% of Redis' used memory; never binding for these fixtures.
const STREAM_BUDGET: usize = 256 << 20;

/// Plain record: marker 0, then `ijson::encode_into`.
#[inline(never)]
fn encode_plain(value: &IValue) -> Vec<u8> {
    let mut bytes = vec![MARKER_INLINE];
    ijson::encode_into(value, &mut bytes);
    bytes
}

/// `rdb_persistence.rs`, `encode_value`.
#[inline(never)]
fn encode_local(value: &IValue) -> Vec<u8> {
    let mut bytes = vec![MARKER_LOCAL];
    let mut encoder = StreamEncoder::new(StreamEncoderConfig { table_budget_bytes: usize::MAX });
    let used = encoder.encode_into(&mut bytes, value);
    let stats = encoder.stats();
    if used == DocUse::default() {
        bytes[0] = MARKER_INLINE;
    } else if stats.saved_bytes <= stats.defs {
        bytes.clear();
        bytes.push(MARKER_INLINE);
        ijson::encode_into(value, &mut bytes);
    }
    bytes
}

/// `rdb_persistence.rs`, `decode_for_load`, for the self-contained markers.
#[inline(never)]
fn decode_record(bytes: &[u8]) -> IValue {
    match bytes.split_first() {
        Some((&MARKER_INLINE, cbor)) => ijson::decode_with_depth_limit(cbor, LIMIT_DEPTH),
        Some((&MARKER_LOCAL, cbor)) => StreamDecoder::new().decode(cbor, LIMIT_DEPTH),
        _ => panic!("{GROUP}: not a self-contained record"),
    }
    .expect("record decodes")
}

/// `SaveSession::encode`: returns the buffer and where the record starts in it.
#[inline(never)]
fn stream_encode(encoder: &mut StreamEncoder, value: &IValue) -> (Vec<u8>, usize) {
    let base_id = encoder.stats().defs as u32;
    encoder.reset_if_needed();
    let mut bytes = vec![MARKER_INLINE; STREAM_HEADER_LEN];
    let used = encoder.encode_into(&mut bytes, value);
    if used == DocUse::default() {
        return (bytes, STREAM_HEADER_LEN - 1);
    }
    bytes[0] = MARKER_STREAM;
    bytes[1..STREAM_HEADER_LEN].copy_from_slice(&base_id.to_le_bytes());
    (bytes, 0)
}

/// `decode_for_load` for a record of a stream, with `decode_stream_document`.
#[inline(never)]
fn stream_decode(decoder: &mut StreamDecoder, record: &[u8]) -> IValue {
    match record.split_first() {
        Some((&MARKER_INLINE, cbor)) => ijson::decode_with_depth_limit(cbor, LIMIT_DEPTH),
        Some((&MARKER_STREAM, rest)) => {
            let (base_id, document) = rest.split_first_chunk().expect("stream header");
            let found = u32::from_le_bytes(*base_id) as usize;
            assert!(found == decoder.stats().defs as usize, "{GROUP}: base id mismatch");
            decoder.decode(document, LIMIT_DEPTH)
        }
        _ => panic!("{GROUP}: not a stream record"),
    }
    .expect("record decodes")
}

fn new_stream_encoder() -> StreamEncoder {
    StreamEncoder::new(StreamEncoderConfig { table_budget_bytes: STREAM_BUDGET })
}

fn check_record(case: &str, record: &[u8], value: &IValue) {
    assert!(record.len() > 1, "{GROUP} {case}: empty record");
    assert!(decode_record(record) == *value, "{GROUP} {case}: decode(encode(v)) differs from v");
}

/// Encode `docs` through one fresh encoder, check the stream decodes back to
/// them through one decoder, and return the records.
fn build_stream(case: &str, docs: &[&IValue]) -> Vec<Vec<u8>> {
    let mut encoder = new_stream_encoder();
    let records: Vec<Vec<u8>> = docs
        .iter()
        .map(|d| {
            let (bytes, start) = stream_encode(&mut encoder, d);
            bytes[start..].to_vec()
        })
        .collect();
    assert!(!encoder.is_poisoned(), "{GROUP} {case}: stream encoder poisoned");
    let mut decoder = StreamDecoder::new();
    for (i, (record, doc)) in records.iter().zip(docs).enumerate() {
        assert!(record.len() > 1, "{GROUP} {case}: empty stream record {i}");
        let back = stream_decode(&mut decoder, record);
        assert!(back == **doc, "{GROUP} {case}: stream document {i} differs after decode");
        assert!(
            ijson_serialize(&back) == ijson_serialize(doc),
            "{GROUP} {case}: stream document {i} serialises differently after decode"
        );
    }
    records
}

/// Time a stream of `docs` through one encoder, and its records through one
/// decoder. `text_len` is the JSON text length of all documents.
fn bench_stream(ctx: &mut Ctx, enc_case: String, dec_case: String, docs: &[&IValue], text_len: usize) {
    let enc_row = Row { group: GROUP, case: enc_case, imp: "stream", work: Work::Bytes(text_len) };
    let dec_probe = Row { group: GROUP, case: dec_case, imp: "stream", work: Work::Bytes(0) };
    if !ctx.wants(&enc_row) && !ctx.wants(&dec_probe) {
        return;
    }
    let records = build_stream(&enc_row.case, docs);
    let encoded_len: usize = records.iter().map(Vec::len).sum();
    let dec_row = Row { work: Work::Bytes(encoded_len), ..dec_probe };

    ctx.bench_with_setup(
        enc_row,
        || (new_stream_encoder(), Vec::<(Vec<u8>, usize)>::with_capacity(docs.len())),
        |(encoder, out)| {
            for d in docs {
                out.push(stream_encode(encoder, black_box(d)));
            }
        },
    );
    ctx.bench_with_setup(
        dec_row,
        || (StreamDecoder::new(), Vec::<IValue>::with_capacity(records.len())),
        |(decoder, out)| {
            for r in &records {
                out.push(stream_decode(decoder, black_box(r)));
            }
        },
    );
}

pub fn run(ctx: &mut Ctx) {
    let fixtures = ctx.fixtures;
    for fx in fixtures {
        let text_len = fx.text.len();
        let row = |case: &str, imp, n| Row { group: GROUP, case: format!("{}/{case}", fx.name), imp, work: Work::Bytes(n) };

        let one_shot = ["cbor", "local", "json"]
            .iter()
            .any(|imp| ctx.wants(&row("encode", imp, 0)) || ctx.wants(&row("decode", imp, 0)));
        if one_shot {
            let iv = ijson_parse(&fx.text);

            let plain = encode_plain(&iv);
            check_record(&fx.name, &plain, &iv);
            assert!(plain[1..] == ijson::encode(&iv)[..], "{GROUP} {}: encode_into differs from encode", fx.name);
            ctx.bench(row("encode", "cbor", text_len), || encode_plain(black_box(&iv)));
            ctx.bench(row("decode", "cbor", plain.len()), || decode_record(black_box(&plain)));

            let local = encode_local(&iv);
            check_record(&fx.name, &local, &iv);
            assert!(local.len() <= plain.len(), "{GROUP} {}: local record larger than plain", fx.name);
            ctx.bench(row("encode", "local", text_len), || encode_local(black_box(&iv)));
            ctx.bench(row("decode", "local", local.len()), || decode_record(black_box(&local)));

            let json = ijson_serialize(&iv);
            assert!(!json.is_empty(), "{GROUP} {}: empty JSON text", fx.name);
            assert!(ijson_parse(&json) == iv, "{GROUP} {}: JSON round trip differs", fx.name);
            ctx.bench(row("encode", "json", text_len), || ijson_serialize(black_box(&iv)));
            ctx.bench(row("decode", "json", json.len()), || ijson_parse(black_box(&json)));
        }

        // Separate parses: distinct containers, shared interned strings.
        let enc = format!("{}/stream-encode-x{STREAM_DOCS}", fx.name);
        let dec = format!("{}/stream-decode-x{STREAM_DOCS}", fx.name);
        let wanted = [&enc, &dec]
            .iter()
            .any(|c| ctx.wants(&Row { group: GROUP, case: (**c).clone(), imp: "stream", work: Work::Bytes(0) }));
        if wanted {
            let docs: Vec<IValue> = (0..STREAM_DOCS).map(|_| ijson_parse(&fx.text)).collect();
            let refs: Vec<&IValue> = docs.iter().collect();
            bench_stream(ctx, enc, dec, &refs, text_len * STREAM_DOCS);
        }
    }

    // Every fixture once through one stream.
    if fixtures.len() > 1 {
        let enc = "all-fixtures/stream-encode".to_string();
        let dec = "all-fixtures/stream-decode".to_string();
        let wanted = [&enc, &dec]
            .iter()
            .any(|c| ctx.wants(&Row { group: GROUP, case: (**c).clone(), imp: "stream", work: Work::Bytes(0) }));
        if wanted {
            let docs: Vec<IValue> = fixtures.iter().map(|fx| ijson_parse(&fx.text)).collect();
            let refs: Vec<&IValue> = docs.iter().collect();
            let text_len = fixtures.iter().map(|fx| fx.text.len()).sum();
            bench_stream(ctx, enc, dec, &refs, text_len);
        }
    }
}

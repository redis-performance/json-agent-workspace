//! Seeded synthetic JSON documents for the shapes and sizes RedisJSON's
//! datasets do not cover.
//!
//! Everything here produces JSON *text*. Generation is deterministic: the
//! same [`Spec`] gives byte-identical text on every platform. No floating
//! point arithmetic or formatting is involved (floats are assembled from
//! integer digits), and the only randomness is a SplitMix64 stream seeded
//! from `Spec::seed`.
//!
//! The shapes target the places where `ijson` changes representation:
//!
//! - strings of up to 7 bytes are stored inline in the value word; longer
//!   ones are interned in the process-wide cache (`unsafe_string.rs`,
//!   `INLINE_STRING_MAX_LEN` and `IString::intern`); this holds for object
//!   keys and string values alike;
//! - objects switch to a hash table above 8 fields (`object.rs`,
//!   `SMALL_OBJECT_THRESHOLD`) and the parser keeps up to 16 fields in an
//!   inline buffer before moving to a pooled map (`de.rs`,
//!   `OBJECT_BUFFER_INLINE_CAPACITY`);
//! - arrays whose elements are all numbers are stored as typed arrays, with
//!   one tag per width class (`array.rs`, `ArrayTag`); any other element
//!   makes the array heterogeneous;
//! - integers in `[-2^61, 2^61)` are inline, wider ones and floats are boxed
//!   (`number.rs`, `INLINE_LOWER`/`INLINE_UPPER`).
//!
//! # Salts
//!
//! [`Spec::key_salt`] and [`Spec::value_salt`] append a fixed-width tag
//! (`#` and 16 hex digits) to every object key, respectively every string
//! value. Two documents from the same spec with different salts have the same
//! structure and length but share no tagged string, by construction rather
//! than by chance, and every tagged string is long enough to be interned.
//! The cold string-cache parse cases are built on this.

use crate::fixtures::Fixture;

/// SplitMix64. Small, seedable, and identical everywhere.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator whose whole stream is determined by `seed`.
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (`n > 0`). The slight modulo bias does not matter here.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

/// Width class of a homogeneous integer array; the element range is the full
/// range of the named type, so `ijson` ends up with the matching `ArrayTag`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntClass {
    /// `-128..=127`
    I8,
    /// `0..=255`
    U8,
    /// `-32768..=32767`
    I16,
    /// `0..=65535`
    U16,
    /// Full `i32` range.
    I32,
    /// Full `u32` range.
    U32,
    /// Full `i64` range (most values are outside the inline integer range).
    I64,
    /// Full `u64` range (about half the values exceed `i64::MAX`).
    U64,
}

/// What the strings of a [`Shape::Strings`] document look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrKind {
    /// Every string distinct and long (tens of bytes): every one is a cache miss.
    UniqueLong,
    /// Long strings drawn from a pool of 16: almost every one is a cache hit.
    Repeated,
    /// 1 to 7 ASCII bytes: stored inline, the cache is never consulted.
    ShortInline,
    /// Long strings dense with JSON escapes (`\"`, `\\`, `\n`, `\uXXXX`,
    /// surrogate pairs), which the parser must unescape into a scratch buffer.
    Escapes,
    /// Long strings of raw multi-byte UTF-8 (2, 3 and 4 byte sequences).
    Unicode,
}

/// The structure of a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// One object with `keys` fields. Keys alternate between inline-length and
    /// interned-length names; values cycle through integers, floats, booleans,
    /// null and short and long strings.
    FlatObject {
        /// Number of fields.
        keys: usize,
    },
    /// An array of small "record" objects with the same nine keys in every
    /// record, grown until the text is at least `target_bytes` long. A record
    /// is roughly 200 bytes.
    Records {
        /// Minimum length of the text, in bytes.
        target_bytes: usize,
    },
    /// `{"child":{"child":{...}}}`, `depth` objects deep, each with one extra
    /// scalar field.
    NestedObjects {
        /// Number of nested objects. Keep below 128, `serde_json`'s recursion limit.
        depth: usize,
    },
    /// `[[[...]]]`, `depth` arrays deep, with one integer at the bottom.
    NestedArrays {
        /// Number of nested arrays. Keep below 128.
        depth: usize,
    },
    /// An array of `len` integers of one width class.
    IntArray {
        /// Number of elements.
        len: usize,
        /// Element range.
        class: IntClass,
    },
    /// An array of `len` decimal floats.
    FloatArray {
        /// Number of elements.
        len: usize,
    },
    /// An array of `len` elements, mostly numbers but with strings, booleans
    /// and nulls mixed in, so it cannot be a typed array.
    MixedArray {
        /// Number of elements.
        len: usize,
    },
    /// An array of `count` strings.
    Strings {
        /// Number of strings.
        count: usize,
        /// What they contain.
        kind: StrKind,
    },
    /// An array of `count` small objects whose fields cover the number forms:
    /// small integer, negative `i64`, `u64` above `i64::MAX`, decimal,
    /// positive and negative exponent.
    Numbers {
        /// Number of objects (six numbers each).
        count: usize,
    },
}

/// A complete, reproducible description of one document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    /// Structure.
    pub shape: Shape,
    /// Seed of the random stream. Same seed, same text.
    pub seed: u64,
    /// If set, a tag derived from this number is appended to every object key.
    pub key_salt: Option<u64>,
    /// If set, a tag derived from this number is appended to every string value.
    pub value_salt: Option<u64>,
}

impl Spec {
    /// A spec with the default seed and no salts.
    pub fn new(shape: Shape) -> Self {
        Spec { shape, seed: 0x5EED_0000_0000_0001, key_salt: None, value_salt: None }
    }

    /// The same document structure with another random stream.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// The same document with every key tagged by `key_salt` and every string
    /// value tagged by `value_salt` (see the module documentation).
    pub fn salted(mut self, key_salt: u64, value_salt: u64) -> Self {
        self.key_salt = Some(key_salt);
        self.value_salt = Some(value_salt);
        self
    }

    /// Produce the JSON text.
    pub fn generate(&self) -> String {
        let tag = |s: Option<u64>| s.map(|n| format!("#{n:016x}")).unwrap_or_default();
        let mut w = Writer {
            out: String::new(),
            rng: Rng::new(self.seed),
            ktag: tag(self.key_salt),
            vtag: tag(self.value_salt),
        };
        match self.shape {
            Shape::FlatObject { keys } => w.flat_object(keys),
            Shape::Records { target_bytes } => w.records(target_bytes),
            Shape::NestedObjects { depth } => w.nested_objects(depth),
            Shape::NestedArrays { depth } => w.nested_arrays(depth),
            Shape::IntArray { len, class } => w.int_array(len, class),
            Shape::FloatArray { len } => w.float_array(len),
            Shape::MixedArray { len } => w.mixed_array(len),
            Shape::Strings { count, kind } => w.strings(count, kind),
            Shape::Numbers { count } => w.numbers(count),
        }
        w.out
    }

    /// Generate the text and check it the way dataset fixtures are checked
    /// (`fixtures::verify`): both parsers accept it and the `ijson` value
    /// serialises back to the document the baseline parsed. Panics otherwise.
    pub fn generate_verified(&self, name: &str) -> String {
        let fx = Fixture { name: name.to_owned(), text: self.generate() };
        if let Err(e) = crate::fixtures::verify(&fx) {
            panic!("synthetic document {name} ({self:?}) failed verification: {e}");
        }
        fx.text
    }
}

/// A named [`Spec`]. The name is stable and short enough to be a case name.
#[derive(Clone, Copy, Debug)]
pub struct Preset {
    /// Stable short name, e.g. `records-10k`.
    pub name: &'static str,
    /// What to generate.
    pub spec: Spec,
    /// Rough size of the text in bytes, for deciding what to skip in a smoke run.
    pub approx_bytes: usize,
}

/// Length of the numeric and string arrays in the fixed-size presets.
const ARRAY_LEN: usize = 4096;

/// Targets of the record-list size sweep: name suffix and bytes.
const SWEEP: &[(&str, usize)] = &[
    ("records-100b", 100),
    ("records-1k", 1_000),
    ("records-10k", 10_000),
    ("records-100k", 100_000),
    ("records-1m", 1_000_000),
    ("records-10m", 10_000_000),
];

/// Every preset, in a fixed order.
pub fn presets() -> Vec<Preset> {
    let mut v = Vec::new();
    let mut add = |name, shape, approx_bytes| v.push(Preset { name, spec: Spec::new(shape), approx_bytes });

    // Flat objects around the 8-field hash-table and 16-field parse-buffer thresholds.
    add("flat-8keys", Shape::FlatObject { keys: 8 }, 200);
    add("flat-9keys", Shape::FlatObject { keys: 9 }, 220);
    add("flat-16keys", Shape::FlatObject { keys: 16 }, 400);
    add("flat-17keys", Shape::FlatObject { keys: 17 }, 420);
    add("flat-64keys", Shape::FlatObject { keys: 64 }, 1_600);
    add("flat-1024keys", Shape::FlatObject { keys: 1024 }, 26_000);

    // Record lists, 100 bytes to 10 MB.
    for &(name, target_bytes) in SWEEP {
        add(name, Shape::Records { target_bytes }, target_bytes);
    }

    // Nesting, below serde_json's recursion limit of 128.
    add("nest-obj-120", Shape::NestedObjects { depth: 120 }, 2_500);
    add("nest-arr-120", Shape::NestedArrays { depth: 120 }, 250);

    // Numeric arrays of one length: one per width class, floats, and heterogeneous.
    add("ints-i8", Shape::IntArray { len: ARRAY_LEN, class: IntClass::I8 }, 14_000);
    add("ints-u8", Shape::IntArray { len: ARRAY_LEN, class: IntClass::U8 }, 14_000);
    add("ints-i16", Shape::IntArray { len: ARRAY_LEN, class: IntClass::I16 }, 24_000);
    add("ints-u16", Shape::IntArray { len: ARRAY_LEN, class: IntClass::U16 }, 24_000);
    add("ints-i32", Shape::IntArray { len: ARRAY_LEN, class: IntClass::I32 }, 44_000);
    add("ints-u32", Shape::IntArray { len: ARRAY_LEN, class: IntClass::U32 }, 44_000);
    add("ints-i64", Shape::IntArray { len: ARRAY_LEN, class: IntClass::I64 }, 82_000);
    add("ints-u64", Shape::IntArray { len: ARRAY_LEN, class: IntClass::U64 }, 82_000);
    add("floats", Shape::FloatArray { len: ARRAY_LEN }, 40_000);
    add("mixed-array", Shape::MixedArray { len: ARRAY_LEN }, 40_000);

    // Strings.
    add("str-unique", Shape::Strings { count: ARRAY_LEN, kind: StrKind::UniqueLong }, 250_000);
    add("str-repeated", Shape::Strings { count: ARRAY_LEN, kind: StrKind::Repeated }, 250_000);
    add("str-inline", Shape::Strings { count: ARRAY_LEN, kind: StrKind::ShortInline }, 28_000);
    add("str-escapes", Shape::Strings { count: ARRAY_LEN, kind: StrKind::Escapes }, 300_000);
    add("str-unicode", Shape::Strings { count: ARRAY_LEN, kind: StrKind::Unicode }, 250_000);

    // Number forms.
    add("nums-mixed", Shape::Numbers { count: 1024 }, 110_000);
    v
}

/// Look a preset up by name.
pub fn preset(name: &str) -> Option<Preset> {
    presets().into_iter().find(|p| p.name == name)
}

/// The presets of the record-list size sweep, smallest first.
pub fn size_sweep() -> Vec<Preset> {
    presets().into_iter().filter(|p| SWEEP.iter().any(|(n, _)| *n == p.name)).collect()
}

const WORDS: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet", "kilo", "lima",
    "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango", "uniform", "victor", "whiskey",
    "xray", "yankee", "zulu",
];
const CITIES: &[&str] = &[
    "Lisbon", "Tel Aviv", "San Francisco", "London", "Bangalore", "Sao Paulo", "Toronto", "Singapore",
];
const TAGS: &[&str] = &["new", "vip", "trial", "blocked", "newsletter", "enterprise", "beta", "eu"];
/// Already JSON-escaped fragments.
const ESCAPES: &[&str] = &[
    "\\\"", "\\\\", "\\/", "\\n", "\\t", "\\r", "\\b", "\\f", "\\u0001", "\\u00e9", "\\u20ac", "\\ud83d\\ude00",
];
const UNICODE: &[&str] = &["é", "ß", "ñ", "я", "ж", "λ", "日", "本", "語", "한", "€", "😀", "🚀", "𝄞"];

struct Writer {
    out: String,
    rng: Rng,
    ktag: String,
    vtag: String,
}

impl Writer {
    /// `"name<key tag>":`
    fn key(&mut self, name: &str) {
        self.out.push('"');
        self.out.push_str(name);
        self.out.push_str(&self.ktag);
        self.out.push_str("\":");
    }

    /// `"body<value tag>"`. `body` must already be valid inside a JSON string.
    fn string(&mut self, body: &str) {
        self.out.push('"');
        self.out.push_str(body);
        self.out.push_str(&self.vtag);
        self.out.push('"');
    }

    fn int(&mut self, v: i128) {
        self.out.push_str(&v.to_string());
    }

    /// A decimal such as `-123.456`, written from integer parts.
    fn decimal(&mut self) {
        let whole = self.rng.below(200_000) as i64 - 100_000;
        let frac = self.rng.below(1_000_000);
        self.out.push_str(&format!("{whole}.{frac:06}"));
    }

    /// A float with an exponent such as `4.25e14` or `-7.5e-9`.
    ///
    /// The exponent is kept small on purpose. `serde_json` without its
    /// `float_roundtrip` feature (the build RedisJSON uses) only converts
    /// decimal text to the nearest `f64` when the power of ten is small; past
    /// that it can be one unit in the last place off, and then parsing the
    /// re-serialised value does not give the original value back, which the
    /// pre-timing verification rejects for either implementation.
    fn exponent(&mut self, negative_exp: bool) {
        let whole = self.rng.below(18) as i64 - 9;
        let frac = self.rng.below(1000);
        let exp = 1 + self.rng.below(15);
        let sign = if negative_exp { "-" } else { "" };
        self.out.push_str(&format!("{whole}.{frac:03}e{sign}{exp}"));
    }

    /// Lowercase words joined by `sep`, `n` of them.
    fn words(&mut self, n: usize, sep: &str) -> String {
        let mut s = String::new();
        for i in 0..n {
            if i > 0 {
                s.push_str(sep);
            }
            s.push_str(self.rng.pick(WORDS));
        }
        s
    }

    fn scalar(&mut self) {
        match self.rng.below(7) {
            0 => {
                let v = self.rng.below(1000) as i128;
                self.int(v)
            }
            1 => {
                let v = self.rng.next_u64() as i64 as i128;
                self.int(v)
            }
            2 => self.decimal(),
            3 => self.out.push_str(if self.rng.below(2) == 0 { "true" } else { "false" }),
            4 => self.out.push_str("null"),
            5 => {
                let s = self.rng.pick(WORDS);
                self.string(&s[..s.len().min(7)])
            }
            _ => {
                let s = self.words(4, " ");
                self.string(&s)
            }
        }
    }

    fn flat_object(&mut self, keys: usize) {
        self.out.push('{');
        for i in 0..keys {
            if i > 0 {
                self.out.push(',');
            }
            // Even fields have names that fit inline, odd ones are interned.
            if i % 2 == 0 {
                self.key(&format!("f{i}"));
            } else {
                self.key(&format!("field_name_{i}"));
            }
            self.scalar();
        }
        self.out.push('}');
    }

    fn record(&mut self, id: usize) {
        let first = self.rng.pick(WORDS);
        let last = self.rng.pick(WORDS);
        self.out.push('{');
        self.key("id");
        self.int(id as i128);
        self.out.push(',');
        self.key("name");
        let n = self.rng.below(1 << 24);
        self.string(&format!("{first} {last} {n:x}"));
        self.out.push(',');
        self.key("email_address");
        let n = self.rng.below(1000);
        self.string(&format!("{first}.{last}{n}@example.com"));
        self.out.push(',');
        self.key("city");
        let city = self.rng.pick(CITIES);
        self.string(city);
        self.out.push(',');
        self.key("active");
        self.out.push_str(if self.rng.below(4) != 0 { "true" } else { "false" });
        self.out.push(',');
        self.key("age");
        let age = 18 + self.rng.below(70);
        self.int(age as i128);
        self.out.push(',');
        self.key("account_balance");
        self.decimal();
        self.out.push(',');
        self.key("last_login_ts");
        let ts = 1_600_000_000_000 + self.rng.below(100_000_000_000);
        self.int(ts as i128);
        self.out.push(',');
        self.key("tags");
        self.out.push('[');
        for i in 0..1 + self.rng.below(3) {
            if i > 0 {
                self.out.push(',');
            }
            let t = self.rng.pick(TAGS);
            self.string(t);
        }
        self.out.push_str("]}");
    }

    fn records(&mut self, target_bytes: usize) {
        self.out.push('[');
        let mut id = 0;
        loop {
            if id > 0 {
                self.out.push(',');
            }
            self.record(id);
            id += 1;
            if self.out.len() + 1 >= target_bytes {
                break;
            }
        }
        self.out.push(']');
    }

    fn nested_objects(&mut self, depth: usize) {
        for level in 0..depth {
            self.out.push('{');
            self.key("level");
            self.int(level as i128);
            self.out.push(',');
            self.key("child");
        }
        self.out.push_str("null");
        for _ in 0..depth {
            self.out.push('}');
        }
    }

    fn nested_arrays(&mut self, depth: usize) {
        for _ in 0..depth {
            self.out.push('[');
        }
        let v = self.rng.below(1000) as i128;
        self.int(v);
        for _ in 0..depth {
            self.out.push(']');
        }
    }

    fn int_of_class(&mut self, class: IntClass) -> i128 {
        let r = self.rng.next_u64();
        match class {
            IntClass::I8 => r as i8 as i128,
            IntClass::U8 => r as u8 as i128,
            IntClass::I16 => r as i16 as i128,
            IntClass::U16 => r as u16 as i128,
            IntClass::I32 => r as i32 as i128,
            IntClass::U32 => r as u32 as i128,
            IntClass::I64 => r as i64 as i128,
            IntClass::U64 => r as i128,
        }
    }

    fn int_array(&mut self, len: usize, class: IntClass) {
        self.out.push('[');
        for i in 0..len {
            if i > 0 {
                self.out.push(',');
            }
            let v = self.int_of_class(class);
            self.int(v);
        }
        self.out.push(']');
    }

    fn float_array(&mut self, len: usize) {
        self.out.push('[');
        for i in 0..len {
            if i > 0 {
                self.out.push(',');
            }
            self.decimal();
        }
        self.out.push(']');
    }

    fn mixed_array(&mut self, len: usize) {
        self.out.push('[');
        for i in 0..len {
            if i > 0 {
                self.out.push(',');
            }
            // The second element is always a string, so the array is
            // heterogeneous from the start whatever the seed.
            if i == 1 {
                self.string("heterogeneous");
            } else {
                self.scalar();
            }
        }
        self.out.push(']');
    }

    fn strings(&mut self, count: usize, kind: StrKind) {
        // The pool for `Repeated` comes from its own stream so that it does
        // not depend on `count`.
        let mut pool_rng = Rng::new(self.rng.next_u64());
        let pool: Vec<String> = (0..16)
            .map(|i| {
                let mut s = format!("pooled-{i:02}");
                for _ in 0..6 {
                    s.push(' ');
                    s.push_str(pool_rng.pick(WORDS));
                }
                s
            })
            .collect();
        self.out.push('[');
        for i in 0..count {
            if i > 0 {
                self.out.push(',');
            }
            match kind {
                StrKind::UniqueLong => {
                    // The index makes each string distinct within the document.
                    let s = format!("{i:06x} {}", self.words(7, " "));
                    self.string(&s);
                }
                StrKind::Repeated => {
                    let s = &pool[self.rng.below(16) as usize];
                    self.out.push('"');
                    self.out.push_str(s);
                    self.out.push_str(&self.vtag);
                    self.out.push('"');
                }
                StrKind::ShortInline => {
                    let w = self.rng.pick(WORDS);
                    let n = 1 + self.rng.below(w.len().min(7) as u64) as usize;
                    self.string(&w[..n]);
                }
                StrKind::Escapes => {
                    let mut s = format!("{i:06x}");
                    for _ in 0..8 {
                        s.push_str(self.rng.pick(ESCAPES));
                        s.push_str(self.rng.pick(WORDS));
                    }
                    self.string(&s);
                }
                StrKind::Unicode => {
                    let mut s = format!("{i:06x}");
                    for _ in 0..20 {
                        s.push_str(self.rng.pick(UNICODE));
                    }
                    self.string(&s);
                }
            }
        }
        self.out.push(']');
    }

    fn numbers(&mut self, count: usize) {
        self.out.push('[');
        for i in 0..count {
            if i > 0 {
                self.out.push(',');
            }
            self.out.push('{');
            self.key("small");
            let v = self.rng.below(100_000) as i128 - 50_000;
            self.int(v);
            self.out.push(',');
            self.key("i64");
            // Below the inline integer range, so it is boxed.
            let v = -(1i128 << 62) - self.rng.below(1 << 60) as i128;
            self.int(v);
            self.out.push(',');
            self.key("u64");
            let v = (1i128 << 63) + self.rng.below(1 << 62) as i128;
            self.int(v);
            self.out.push(',');
            self.key("dec");
            self.decimal();
            self.out.push(',');
            self.key("exp");
            self.exponent(false);
            self.out.push(',');
            self.key("nexp");
            self.exponent(true);
            self.out.push('}');
        }
        self.out.push(']');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ijson_parse, serde_parse};

    #[test]
    fn rng_is_the_reference_splitmix64() {
        // First outputs for seed 0 from the reference implementation.
        let mut r = Rng::new(0);
        assert_eq!(r.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(r.next_u64(), 0x6E78_9E6A_A1B9_65F4);
    }

    #[test]
    fn same_spec_same_text() {
        for p in presets() {
            if p.approx_bytes > 2_000_000 {
                continue;
            }
            assert_eq!(p.spec.generate(), p.spec.generate(), "{}", p.name);
            let salted = p.spec.salted(3, 4);
            assert_eq!(salted.generate(), salted.generate(), "{} salted", p.name);
        }
    }

    #[test]
    fn seed_changes_text() {
        let p = preset("records-10k").unwrap();
        assert_ne!(p.spec.generate(), p.spec.with_seed(7).generate());
    }

    #[test]
    fn preset_names_are_unique() {
        let mut names: Vec<_> = presets().iter().map(|p| p.name).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n);
    }

    #[test]
    fn every_preset_parses_and_round_trips() {
        for p in presets() {
            let text = p.spec.generate_verified(p.name);
            // Both entry points accept it on their own as well.
            drop(ijson_parse(&text));
            drop(serde_parse(&text));
            // Salted variants are the documents the cold-cache cases parse.
            p.spec.salted(0, 1).with_seed(p.spec.seed).generate_verified(p.name);
        }
    }

    #[test]
    fn sizes_are_near_their_targets() {
        for p in presets() {
            let n = p.spec.generate().len();
            assert!(
                n * 2 >= p.approx_bytes && n <= p.approx_bytes * 2 + 128,
                "{}: {} bytes, expected about {}",
                p.name,
                n,
                p.approx_bytes
            );
        }
        // The sweep itself is held to its targets more tightly.
        for p in size_sweep() {
            let Shape::Records { target_bytes } = p.spec.shape else { panic!() };
            let n = p.spec.generate().len();
            assert!(n >= target_bytes && n <= target_bytes + 300, "{}: {} bytes", p.name, n);
        }
    }

    #[test]
    fn salts_keep_length_and_make_strings_disjoint() {
        let base = preset("records-1k").unwrap().spec;
        let a = base.salted(0, 1).generate();
        let b = base.salted(0, 2).generate();
        let c = base.salted(5, 2).generate();
        assert_eq!(a.len(), b.len());
        assert_eq!(a.len(), c.len());
        assert_ne!(a, b);
        // Every string value carries its document's tag and no other.
        assert!(a.contains("#0000000000000001\"") && !a.contains("#0000000000000002"));
        // Keys are tagged separately from values.
        assert!(a.contains("\"name#0000000000000000\":"));
        assert!(c.contains("\"name#0000000000000005\":"));
    }

    #[test]
    fn nesting_stays_under_the_recursion_limit() {
        for p in presets() {
            match p.spec.shape {
                Shape::NestedObjects { depth } | Shape::NestedArrays { depth } => assert!(depth < 128),
                _ => {}
            }
        }
    }
}

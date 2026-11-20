use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::{Compression, write::GzEncoder};
use nosco_crf::{CrfError, CrfModel, DecodeScratch, ModelType, Pattern, parse_hex_float};

const MODEL: &str = concat!(
    "#mdl#2#10\n#rdr#0/0/0\n#qrk#2\n1:A,\n1:B,\n#qrk#4\n",
    "7:u:start,\n5:*next,\n6:b:next,\n6:u:zero,\n",
    "12=1.0\n0=0x1p+0\n2=0x1p-1\n3=0x1.4p+0\n5=0x1p+1\n",
    "6=0x1.8p-1\n9=0x1p-2\n11=0x1p-1\n12=-0x0p+0\n13=0.0\n",
);

fn load() -> CrfModel {
    CrfModel::from_wapiti_reader(MODEL.as_bytes()).unwrap()
}

fn assert_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() <= 1e-14, "{actual} != {expected}");
}

#[test]
fn hex_float_boundaries_and_rounding() {
    for (text, bits) in [
        ("-0x1.234567p-3", (-0.14222221821546555f64).to_bits()),
        ("0x0p+0", 0),
        ("-0x000.000p-4000", 1 << 63),
        ("+0X.8P2", 2.0f64.to_bits()),
        ("0x0.0000000000001p-1022", 1),
        ("0x1p-1075", 0),
        ("0x1.000000000000000000001p-1075", 1),
        ("0x3p-1075", 2),
        ("0x0.fffffffffffff8p-1022", f64::MIN_POSITIVE.to_bits()),
        ("0x1.00000000000008p0", 1.0f64.to_bits()),
        ("0x1.000000000000080000001p0", 1.0f64.to_bits() + 1),
        ("0x1.00000000000018p0", 1.0f64.to_bits() + 2),
        ("0x1.fffffffffffffp1023", f64::MAX.to_bits()),
        ("0x1.fffffffffffff8p1023", f64::INFINITY.to_bits()),
        (
            "0x1p+999999999999999999999999999999999999999999",
            f64::INFINITY.to_bits(),
        ),
        ("0x1p-999999999999999999999999999999999999999999", 0),
        (" \t-5.5e-1\r\n", (-0.55f64).to_bits()),
    ] {
        assert_eq!(parse_hex_float(text).unwrap().to_bits(), bits, "{text}");
    }
    for text in [
        "",
        "abc",
        "0x.p0",
        "0x1p",
        "0x1p+",
        "0x1p0x",
        "0x1.2.3p4",
        "0x1p- 2",
    ] {
        assert!(parse_hex_float(text).is_err(), "{text}");
    }
}

#[test]
fn hex_float_roundtrips_binary64_bit_patterns() {
    let mut state = 0x243f_6a88_85a3_08d3u64;
    for _ in 0..10_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let fraction = state & ((1 << 52) - 1);
        let exponent = ((state >> 52) & 0x7ff) as i32;
        if exponent == 0x7ff {
            continue;
        }
        let sign = if state >> 63 == 0 { "" } else { "-" };
        let text = if exponent == 0 {
            format!("{sign}0x0.{fraction:013x}p-1022")
        } else {
            format!("{sign}0x1.{fraction:013x}p{}", exponent - 1023)
        };
        assert_eq!(parse_hex_float(&text).unwrap().to_bits(), state, "{text}");
    }
}

#[test]
fn pattern_offsets_case_and_boundaries() {
    let rows: &[&[&str]] = &[&["ALPHA", "large"], &["βETA", "middle"], &["GAMMA", "last"]];
    let pat = Pattern::compile("UKey:%X[@1,0]/%x[@-1,1]/%X[0,0]/%x[@0,0]").unwrap();
    let mut output = Vec::new();
    pat.execute(rows, 1, &mut output).unwrap();
    assert_eq!(output, "uKey:alpha/last/βeta/_x-1".as_bytes());

    let pat = Pattern::compile("b:%x[-4,0]/%x[-5,0]/%x[+0x6,0]/%x[07,0]").unwrap();
    pat.execute(rows, 0, &mut output).unwrap();
    assert_eq!(output, b"b:_x-4/_x-#/_x+4/_x+#");
    let pat = Pattern::compile("*:%x[0,2]").unwrap();
    assert!(matches!(
        pat.execute(rows, 1, &mut output),
        Err(CrfError::MissingColumn { row: 1, column: 2 })
    ));
    for malformed in [
        "",
        "a",
        "u%z[0,0]",
        "u%x[1,]",
        "u%x[0,0",
        "u%t[0,0,\"a**\"]",
    ] {
        assert!(Pattern::compile(malformed).is_err(), "{malformed}");
    }
}

#[test]
fn wapiti_regex_classes_and_matching_order() {
    let cases = [
        (r#"u:%m[0,0,"a*"]"#, "aaa", &b"u:"[..]),
        (r#"u:%m[0,0,"^a*$"]"#, "aaa", b"u:aaa"),
        (r#"u:%m[0,0,"ab?"]"#, "zzabbb", b"u:ab"),
        (r#"u:%m[0,0,"ab?b"]"#, "ab", b"u:ab"),
        (r#"u:%m[0,0,"ab?bc"]"#, "abcX", b"u:abcX"),
        (r#"u:%m[0,0,"ab?b"]/tail"#, "ab", b"u:ab"),
        (r#"u:%M[0,0,"\u\u?"]"#, "--ABC", b"u:ab"),
        (r#"u:%M[0,0,"\u"]"#, "ab", b"u:"),
        (r#"u:%T[0,0,"^$"]/%t[0,0,"$"]"#, "abc", b"u:false/true"),
        (r#"u:%t[0,0,"^\a\d\l\p\s\u\w$"]"#, "A2b!\x0bZ3", b"u:true"),
        (r#"u:%t[0,0,"^\A\D\L\P\S\U\W$"]"#, "1B1a1a!", b"u:true"),
        (r#"u:%m[0,0,"\$\"\?\*\\"]"#, "$\"?*\\", b"u:$\"?*\\"),
        (r#"u:%m[0,0,"."]"#, "β", b"u:\xce"),
    ];
    let mut output = Vec::new();
    for (pattern, token, expected) in cases {
        Pattern::compile(pattern)
            .unwrap()
            .execute(&[&[token]], 0, &mut output)
            .unwrap();
        assert_eq!(output, expected, "{pattern}");
    }
}

#[test]
fn wapiti_netstrings_use_byte_lengths_and_keep_observations_as_bytes() {
    let fixture =
        b"#mdl#1\n#rdr#1/0\n13:u:%m[0,0,\".\"],\n#qrk#1\n4:\xce\xbb,\n,\n#qrk#1\n3:u:\xce,\n0=2\n";
    let model = CrfModel::from_wapiti_reader(fixture.as_slice()).unwrap();
    assert_eq!(model.labels(), &["λ,\n"]);
    assert_eq!(model.model_type(), ModelType::MaxEnt);
    assert_eq!(
        model
            .tag_with_scores(&[&["β"]], &mut DecodeScratch::new())
            .unwrap()
            .sequence_score,
        2.0
    );
    assert!(CrfModel::from_wapiti_reader(&b"#mdl#1\n#rdr#0/0\n#qrk#1\n8:A,\n"[..]).is_err());
    assert!(CrfModel::from_wapiti_reader(&b"#mdl#1\n#rdr#0/0\n#qrk#1\n1:A.\n"[..]).is_err());
}

#[test]
fn crf_viterbi_scores_and_scratch_reuse() {
    fn require_send_sync<T: Send + Sync>() {}
    require_send_sync::<CrfModel>();
    let model = load();
    let stats = model.stats();
    assert_eq!(stats.num_observations, 3);
    assert_eq!(stats.num_unigram_weights, 3);
    assert_eq!(stats.num_bigram_weights, 4);
    assert_eq!(stats.hash_capacity, 8);
    assert_eq!(model.label_name(1), "B");
    let mut scratch = DecodeScratch::new();

    let result = model
        .tag_with_scores(&[&["u:start"], &["*next", "b:next"]], &mut scratch)
        .unwrap();
    assert_eq!(result.label_ids, [0, 1]);
    assert_eq!(result.sequence_score, 4.5);
    assert_eq!(result.token_scores, [1.0, 3.5]);

    let single = model
        .tag_with_scores(&[&["*next", "b:next"]], &mut scratch)
        .unwrap();
    assert_eq!(single.label_ids, [1]);
    assert_eq!(single.sequence_score, 1.25);
    assert_eq!(
        model
            .tag_with_scores(&[], &mut scratch)
            .unwrap()
            .sequence_score,
        0.0
    );
    assert_eq!(
        model
            .tag_slices(&[&["u:unknown"], &["b:unknown"]], &mut scratch)
            .unwrap(),
        [0, 0]
    );
    assert!(matches!(
        model.tag_slices(&[&["invalid"]], &mut scratch),
        Err(CrfError::InvalidObservation { .. })
    ));

    let rows = [
        vec![String::from("u:start")],
        vec!["*next".to_owned(), "b:next".to_owned()],
    ];
    assert_eq!(model.tag(&rows, &mut scratch).unwrap(), [0, 1]);
    assert_eq!(
        model
            .tag_lines(&[" \tu:start", "*next\x0bb:next \r"], &mut scratch)
            .unwrap(),
        [0, 1]
    );
    assert_eq!(
        model
            .tag_with_scores(&[&["u:start", "u:start"]], &mut scratch)
            .unwrap()
            .sequence_score,
        2.0
    );
}

#[test]
fn memm_and_posterior_match_explicit_path_probabilities() {
    let mut scratch = DecodeScratch::new();
    let rows: &[&[&str]] = &[&["u:start"], &["*next", "b:next"]];
    let model = load();
    let posterior = model.tag_posterior(rows, &mut scratch).unwrap();
    let [aa, ab, ba, bb] = [1.5f64, 4.5, 1.25, 1.75].map(f64::exp);
    let z = aa + ab + ba + bb;
    assert_eq!(posterior.label_ids, [0, 1]);
    assert_close(posterior.token_scores[0], (aa + ab) / z);
    assert_close(posterior.token_scores[1], (ab + bb) / z);
    assert_close(posterior.sequence_score, (aa + ab) * (ab + bb) / (z * z));
    assert_eq!(
        model
            .tag_posterior(&[], &mut scratch)
            .unwrap()
            .sequence_score,
        1.0
    );

    let memm = MODEL.replacen("#mdl#2#", "#mdl#1#", 1);
    let model = CrfModel::from_wapiti_reader(memm.as_bytes()).unwrap();
    let conditional = model.tag_with_scores(rows, &mut scratch).unwrap();
    let initial = 1.0 / (1.0 + (-1.0f64).exp());
    let transition = 1.0 / (1.0 + (-3.0f64).exp());
    assert_eq!(conditional.label_ids, [0, 1]);
    assert_close(conditional.token_scores[0], initial);
    assert_close(conditional.token_scores[1], transition);
    assert_close(conditional.sequence_score, initial * transition);
    assert_eq!(
        model.tag_posterior(rows, &mut scratch).unwrap(),
        conditional
    );
}

#[test]
fn autouni_and_zero_weight_models() {
    let model = CrfModel::from_wapiti_reader(
        &b"#mdl#0#1\n#rdr#0/0/1\n#qrk#2\n1:A,\n1:B,\n#qrk#1\n4:ufoo,\n1=3\n"[..],
    )
    .unwrap();
    assert_eq!(
        model
            .tag_lines(&["foo", "bar"], &mut DecodeScratch::new())
            .unwrap(),
        [1, 0]
    );

    let model = CrfModel::from_wapiti_reader(
        &b"#mdl#0\n#rdr#0/0\n#qrk#2\n1:A,\n1:B,\n#qrk#1\n4:ufoo,\n"[..],
    )
    .unwrap();
    assert_eq!(model.stats().num_observations, 0);
    let row: &[&str] = &["ufoo"];
    let rows = vec![row; 1_100];
    let result = model
        .tag_posterior(&rows, &mut DecodeScratch::new())
        .unwrap();
    assert!(result.label_ids.iter().all(|&id| id == 0));
    assert!(result.token_scores.iter().all(|&score| score == 0.5));
    assert_eq!(result.sequence_score, 0.0);
}

#[test]
fn binary_roundtrip_and_corruption() {
    let model = load();
    let mut bytes = Vec::new();
    model.write_gcrf(&mut bytes).unwrap();
    assert_eq!(&bytes[..8], b"GCRF\x01\x00\x00\x00");
    let restored = CrfModel::from_gcrf_bytes(&bytes).unwrap();
    let mut second = Vec::new();
    restored.write_gcrf(&mut second).unwrap();
    assert_eq!(bytes, second);
    let rows: &[&[&str]] = &[&["u:start"], &["*next", "b:next"]];
    let mut scratch = DecodeScratch::new();
    assert_eq!(
        model.tag_with_scores(rows, &mut scratch).unwrap(),
        restored.tag_with_scores(rows, &mut scratch).unwrap()
    );
    assert_eq!(model.stats(), restored.stats());
    for i in 0..bytes.len() {
        assert!(
            CrfModel::from_gcrf_bytes(&bytes[..i]).is_err(),
            "truncated to {i}"
        );
    }
    bytes[4] = 2;
    assert!(matches!(
        CrfModel::from_gcrf_bytes(&bytes),
        Err(CrfError::UnsupportedVersion(2))
    ));
    bytes[4] = 1;
    bytes[34] ^= 1;
    assert!(matches!(
        CrfModel::from_gcrf_bytes(&bytes),
        Err(CrfError::ChecksumMismatch)
    ));
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nosco-crf-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn path_auto_loading_and_gzip() {
    let dir = TempDir::new();
    let path = dir.0.join("model.without-a-format-extension");
    fs::write(&path, MODEL).unwrap();
    let model = CrfModel::load_auto(&path).unwrap();

    let mut gzip = GzEncoder::new(Vec::new(), Compression::fast());
    gzip.write_all(MODEL.as_bytes()).unwrap();
    let gzip = gzip.finish().unwrap();
    fs::write(&path, &gzip).unwrap();
    assert_eq!(model.stats(), CrfModel::load_auto(&path).unwrap().stats());
    assert_eq!(model.stats(), CrfModel::load_wapiti(&path).unwrap().stats());
    // Exercise a forward-only source that delivers the magic in separate reads.
    struct OneByte<'a>(&'a [u8]);
    impl Read for OneByte<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let count = bytes.len().min(1);
            self.0.read(&mut bytes[..count])
        }
    }
    assert_eq!(
        model.stats(),
        CrfModel::from_wapiti_reader(OneByte(&gzip))
            .unwrap()
            .stats()
    );

    model.save_gcrf(&path).unwrap();
    for restored in [
        CrfModel::load_auto(&path),
        CrfModel::load_gcrf(&path),
        CrfModel::load_gcrf_mmap(&path),
    ] {
        assert_eq!(model.stats(), restored.unwrap().stats());
    }
}

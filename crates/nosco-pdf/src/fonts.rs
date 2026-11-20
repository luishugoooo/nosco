use std::{borrow::Cow, collections::HashMap, sync::OnceLock};

use lopdf::{Dictionary, Document, Object};

use crate::{
    cmap::CMap,
    interpreter::{entry, number, resolve, stream_bytes},
};

pub(crate) type Color = (u8, u8, u8);

pub(crate) struct Font {
    pub name: String,
    pub bold: bool,
    pub italic: bool,
    pub ascent: f64,
    pub descent: f64,
    pub vertical: bool,
    simple: Option<Box<[SimpleGlyph; 256]>>,
    unicode: CMap,
    encoding: CMap,
    default_width: f64,
    widths: Vec<MetricRange<f64>>,
    vertical_metrics: Vec<MetricRange<[f64; 3]>>,
    identity: bool,
    cid_unicode: Option<CidUnicode>,
    default_vertical: [f64; 2],
}

#[derive(Default)]
struct SimpleGlyph {
    text: String,
    width: f64,
}

struct MetricRange<T> {
    start: u32,
    end: u32,
    values: Vec<T>,
}

impl<T: Copy> MetricRange<T> {
    fn lookup(&self, code: u32) -> Option<T> {
        if code < self.start || code > self.end {
            return None;
        }
        self.values
            .get(if self.values.len() == 1 {
                0
            } else {
                (code - self.start) as usize
            })
            .copied()
    }
}

pub(crate) struct Decoded<'a> {
    pub text: Cow<'a, str>,
    pub width: f64,
    pub vertical_metric: [f64; 3],
    pub byte_len: usize,
    pub word_space: bool,
}

impl Font {
    pub(crate) fn new(doc: &Document, font: &Dictionary, resource_name: &[u8]) -> Self {
        let get = |key: &[u8]| entry(doc, font, key);
        let subtype = get(b"Subtype").and_then(|object| object.as_name().ok());
        let composite = subtype == Some(b"Type0");
        let descendant = get(b"DescendantFonts")
            .and_then(|obj| obj.as_array().ok()?.first())
            .and_then(|obj| resolve(doc, obj)?.as_dict().ok())
            .unwrap_or(font);
        let descriptor =
            entry(doc, descendant, b"FontDescriptor").and_then(|obj| obj.as_dict().ok());
        let detail = |key: &[u8]| descriptor.and_then(|dict| entry(doc, dict, key));
        let descriptor_name = detail(b"FontName").and_then(|obj| obj.as_name().ok());
        let font_name = get(b"BaseFont")
            .or_else(|| entry(doc, descendant, b"BaseFont"))
            .and_then(|obj| obj.as_name().ok())
            .or(descriptor_name)
            .unwrap_or(resource_name);
        let name = String::from_utf8_lossy(remove_subset_prefix(font_name)).into_owned();
        let style = format!(
            "{name} {}",
            String::from_utf8_lossy(descriptor_name.unwrap_or_default())
        )
        .to_ascii_lowercase();
        let flags = detail(b"Flags")
            .and_then(|obj| obj.as_i64().ok())
            .unwrap_or(0);
        let bold = flags & (1 << 18) != 0
            || detail(b"FontWeight")
                .and_then(number)
                .is_some_and(|weight| weight >= 650.0)
            || [
                "bold",
                "black",
                "heavy",
                "demi",
                "cmbx",
                "cmbsy",
                "sfbx",
                "lmbx",
                "lmsansbold",
            ]
            .iter()
            .any(|part| style.contains(part));
        let italic = flags & (1 << 6) != 0
            || detail(b"ItalicAngle")
                .and_then(number)
                .is_some_and(|angle| angle != 0.0)
            || [
                "italic", "oblique", "slanted", "cmti", "cmsl", "cmmi", "sfti", "sfsl", "lmri",
                "lmmi",
            ]
            .iter()
            .any(|part| style.contains(part));

        let standard = standard_fonts().get(standard_name(&name).as_ref());
        let ascent = detail(b"Ascent")
            .and_then(number)
            .filter(|value| *value > 0.0 && *value < 2500.0)
            .unwrap_or_else(|| standard.map_or(800.0, |metrics| metrics.ascent));
        let descent = detail(b"Descent")
            .and_then(number)
            .map(|value| -value.abs())
            .filter(|value| *value > -1000.0)
            .unwrap_or_else(|| standard.map_or(-200.0, |metrics| metrics.descent));

        let encoding_obj = get(b"Encoding");
        let encoding = parse_cmap(doc, encoding_obj, 0);
        let encoding_name = encoding_obj
            .and_then(|obj| obj.as_name().ok())
            .unwrap_or_default();
        let unicode = parse_cmap(doc, get(b"ToUnicode"), 0);
        let vertical = composite && (encoding.vertical || encoding_name.ends_with(b"-V"));
        let identity = matches!(encoding_name, b"Identity-H" | b"Identity-V");
        let cid = |key: &[u8]| entry(doc, descendant, key);
        let default_width = cid(b"DW").and_then(number).unwrap_or(1000.0);
        let default_vertical = cid(b"DW2")
            .and_then(|obj| obj.as_array().ok())
            .and_then(|array| Some([number(array.first()?)?, number(array.get(1)?)?]))
            .unwrap_or([880.0, -1000.0]);
        let simple =
            (!composite).then(|| simple_font(doc, font, encoding_obj, descriptor, &name, standard));

        let cid_unicode = if composite
            && !unicode.has_unicode()
            && let Some(program) = detail(b"FontFile2")
                .or_else(|| detail(b"FontFile3"))
                .and_then(|obj| obj.as_stream().ok())
            && let Ok(bytes) = stream_bytes(doc, program)
            && let Some(glyphs) = crate::truetype::unicode_by_glyph(&bytes)
        {
            let cid_to_gid = cid(b"CIDToGIDMap")
                .and_then(|obj| obj.as_stream().ok())
                .and_then(|stream| stream_bytes(doc, stream).ok())
                .map(|bytes| {
                    bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| u16::from_be_bytes(*pair))
                        .collect()
                });
            Some(CidUnicode { glyphs, cid_to_gid })
        } else {
            None
        };
        Self {
            name,
            bold,
            italic,
            ascent: ascent / 1000.0,
            descent: descent / 1000.0,
            vertical,
            simple,
            unicode,
            encoding,
            default_width,
            widths: read_metrics::<1>(doc, cid(b"W"))
                .into_iter()
                .map(|range| MetricRange {
                    start: range.start,
                    end: range.end,
                    values: range.values.into_iter().map(|[width]| width).collect(),
                })
                .collect(),
            vertical_metrics: read_metrics::<3>(doc, cid(b"W2")),
            default_vertical,
            identity,
            cid_unicode,
        }
    }

    pub(crate) fn space_width(&self) -> f64 {
        self.simple.as_ref().map_or_else(
            || {
                let width = self.decode(&[0, 32]).width;
                // A missing CID 32 may resolve to the full-width default.
                if width >= 850.0 { 250.0 } else { width }
            },
            |glyphs| glyphs[32].width,
        )
    }

    pub(crate) fn decode(&self, bytes: &[u8]) -> Decoded<'_> {
        let math_sym = is_math_symbol_font(&self.name);
        if let Some(table) = &self.simple {
            let code = bytes[0];
            let glyph = &table[usize::from(code)];
            let text = self
                .unicode
                .unicode(&bytes[..1])
                .map(|text| fix_decoded_text(text, math_sym))
                .unwrap_or_else(|| fix_decoded_text(glyph.text.as_str().into(), math_sym));
            return Decoded {
                text,
                width: glyph.width,
                vertical_metric: [0.0; 3],
                byte_len: 1,
                word_space: code == 32,
            };
        }
        let byte_len = if self.identity {
            bytes.len().min(2)
        } else {
            self.encoding
                .code_len(bytes)
                .or_else(|| self.unicode.code_len(bytes))
                .unwrap_or_else(|| bytes.len().min(2))
        };
        let source = &bytes[..byte_len];
        let value = source
            .iter()
            .fold(0, |value, &byte| (value << 8) | u32::from(byte));
        let cid = self.encoding.cid(source).unwrap_or(value);
        let width = self
            .widths
            .iter()
            .rev()
            .find_map(|range| range.lookup(cid))
            .unwrap_or(self.default_width);
        let text = self
            .unicode
            .unicode(source)
            .map(|text| fix_decoded_text(text, math_sym))
            .unwrap_or_else(|| {
                let character = self
                    .cid_unicode
                    .as_ref()
                    .and_then(|map| map.get(cid))
                    .or_else(|| char::from_u32(value))
                    .filter(|ch| !ch.is_control())
                    .unwrap_or(char::REPLACEMENT_CHARACTER);
                fix_decoded_text(normalize([character]).into(), math_sym)
            });
        let vertical_metric = if self.vertical {
            self.vertical_metrics
                .iter()
                .rev()
                .find_map(|range| range.lookup(cid))
                .unwrap_or([
                    self.default_vertical[1],
                    width / 2.0,
                    self.default_vertical[0],
                ])
        } else {
            [0.0; 3]
        };
        Decoded {
            text,
            width,
            vertical_metric,
            byte_len,
            word_space: source == [32],
        }
    }
}

struct CidUnicode {
    glyphs: Box<[Option<char>]>,
    cid_to_gid: Option<Vec<u16>>,
}

impl CidUnicode {
    fn get(&self, cid: u32) -> Option<char> {
        let index = usize::try_from(cid).ok()?;
        let gid = match &self.cid_to_gid {
            Some(map) => usize::from(*map.get(index)?),
            None => index,
        };
        self.glyphs.get(gid).copied().flatten()
    }
}

fn read_metrics<const N: usize>(
    doc: &Document,
    obj: Option<&Object>,
) -> Vec<MetricRange<[f64; N]>> {
    let Some(array) = obj.and_then(|obj| obj.as_array().ok()) else {
        return Vec::new();
    };
    let mut items = array.iter().filter_map(|obj| resolve(doc, obj));
    let mut ranges = Vec::new();
    let code = |obj: &Object| u32::try_from(obj.as_i64().ok()?).ok();
    while let (Some(start), Some(next)) = (items.next().and_then(code), items.next()) {
        if let Ok(list) = next.as_array() {
            let values: Vec<_> = list
                .as_chunks::<N>()
                .0
                .iter()
                .map(|chunk| {
                    std::array::from_fn(|index| {
                        resolve(doc, &chunk[index]).and_then(number).unwrap_or(0.0)
                    })
                })
                .collect();
            if !values.is_empty() {
                let end = start.saturating_add((values.len() - 1) as u32);
                ranges.push(MetricRange { start, end, values });
            }
        } else if let Some(end) = code(next) {
            let mut values = [0.0; N];
            for value in &mut values {
                let Some(metric) = items.next().and_then(number) else {
                    return ranges;
                };
                *value = metric;
            }
            if start <= end {
                ranges.push(MetricRange {
                    start,
                    end,
                    values: vec![values],
                });
            }
        }
    }
    ranges
}

fn parse_cmap(doc: &Document, object: Option<&Object>, depth: usize) -> CMap {
    if depth >= 8 {
        return CMap::default();
    }
    if let Some(name) = object.and_then(|obj| obj.as_name().ok()) {
        return match name {
            b"Identity-H" => CMap::identity(false),
            b"Identity-V" => CMap::identity(true),
            _ => CMap::default(),
        };
    }
    let Some(stream) = object.and_then(|obj| obj.as_stream().ok()) else {
        return CMap::default();
    };
    let mut parent = parse_cmap(doc, entry(doc, &stream.dict, b"UseCMap"), depth + 1);
    if entry(doc, &stream.dict, b"WMode").and_then(|obj| obj.as_i64().ok()) == Some(1) {
        parent.vertical = true;
    }
    match stream_bytes(doc, stream) {
        Ok(bytes) => CMap::parse(&bytes, parent),
        Err(_) => parent,
    }
}

fn simple_font(
    doc: &Document,
    font: &Dictionary,
    encoding: Option<&Object>,
    descriptor: Option<&Dictionary>,
    name: &str,
    standard: Option<&StandardMetrics>,
) -> Box<[SimpleGlyph; 256]> {
    let encoding_dict = encoding.and_then(|obj| obj.as_dict().ok());
    let base = encoding
        .and_then(|obj| obj.as_name().ok())
        .or_else(|| entry(doc, encoding_dict?, b"BaseEncoding")?.as_name().ok());
    let subtype = entry(doc, font, b"Subtype").and_then(|obj| obj.as_name().ok());
    let flags = descriptor
        .and_then(|dict| entry(doc, dict, b"Flags"))
        .and_then(|obj| obj.as_i64().ok())
        .unwrap_or(0);
    let fallback = if matches!(name, "Symbol" | "SymbolMT") {
        b"Symbol".as_slice()
    } else if subtype == Some(b"TrueType") && flags & 32 != 0 {
        b"WinAnsiEncoding".as_slice()
    } else {
        b"StandardEncoding".as_slice()
    };
    let names = encodings();
    let mut glyphs = if base == Some(b"ZapfDingbats") || (base.is_none() && name == "ZapfDingbats")
    {
        standard_fonts()["ZapfDingbats"].codes.map(str::as_bytes)
    } else {
        *names
            .get(base.unwrap_or(fallback))
            .unwrap_or(&names[fallback])
    };
    // For Type 1 fonts, the original font program defines the built-in encoding.
    // The cleartext /Encoding precedes the encrypted charstrings in PFA/PFB.
    let embedded = |key: &[u8]| {
        if base.is_some() {
            return None;
        }
        let stream = entry(doc, descriptor?, key)?.as_stream().ok()?;
        stream_bytes(doc, stream).ok()
    };
    let type1 = embedded(b"FontFile");
    let cff = if type1.is_none() {
        embedded(b"FontFile3")
    } else {
        None
    };
    if let Some(bytes) = type1.as_deref() {
        embedded_type1_encoding(bytes, &mut glyphs);
        if base.is_none()
            && let Some(tex) = tex_encoding(name)
        {
            for (slot, fallback_glyph) in glyphs.iter_mut().zip(tex) {
                if *slot == b".notdef" || *slot == b"-" {
                    *slot = fallback_glyph;
                }
            }
        }
    } else if let Some(encoding) = cff.as_deref().and_then(crate::cff::encoding) {
        match encoding {
            crate::cff::Encoding::Standard => glyphs = names[b"StandardEncoding".as_slice()],
            crate::cff::Encoding::Expert => glyphs = names[b"ExpertEncoding".as_slice()],
            crate::cff::Encoding::Custom(codes) => {
                for (destination, source) in glyphs.iter_mut().zip(*codes) {
                    if let Some(source) = source {
                        *destination = source;
                    }
                }
            }
        }
    } else if base.is_none()
        && let Some(tex) = tex_encoding(name)
    {
        glyphs = tex;
    }
    if let Some(differences) = encoding_dict
        .and_then(|dict| entry(doc, dict, b"Differences"))
        .and_then(|obj| obj.as_array().ok())
    {
        let mut code = 256;
        for object in differences.iter().filter_map(|obj| resolve(doc, obj)) {
            if let Ok(value) = object.as_i64() {
                code = usize::try_from(value).unwrap_or(256);
            } else if let Ok(glyph) = object.as_name() {
                if let Some(slot) = glyphs.get_mut(code) {
                    *slot = glyph;
                }
                code = code.saturating_add(1);
            }
        }
    }

    let first = entry(doc, font, b"FirstChar")
        .and_then(|obj| obj.as_i64().ok())
        .unwrap_or(0);
    let last = entry(doc, font, b"LastChar")
        .and_then(|obj| obj.as_i64().ok())
        .unwrap_or(255);
    let widths = entry(doc, font, b"Widths").and_then(|obj| obj.as_array().ok());
    let missing = descriptor
        .and_then(|dict| entry(doc, dict, b"MissingWidth"))
        .and_then(number);
    let type3_scale = if subtype == Some(b"Type3") {
        entry(doc, font, b"FontMatrix")
            .and_then(|obj| obj.as_array().ok()?.first())
            .and_then(number)
            .unwrap_or(0.001)
            .abs()
            * 1000.0
    } else {
        1.0
    };
    Box::new(std::array::from_fn(|code| {
        let glyph = glyphs[code];
        let text = glyph_text(glyph).unwrap_or_else(|| {
            if code == 32 {
                " ".into()
            } else {
                char::REPLACEMENT_CHARACTER.to_string()
            }
        });
        let width = (code as i64 <= last)
            .then_some(widths)
            .flatten()
            .and_then(|values| {
                usize::try_from((code as i64).checked_sub(first)?)
                    .ok()
                    .and_then(|index| values.get(index))
            })
            .and_then(|obj| resolve(doc, obj))
            .and_then(number)
            .map(|width| width * type3_scale)
            .or(missing)
            .or_else(|| {
                standard?
                    .widths
                    .get(std::str::from_utf8(glyph).ok()?)
                    .copied()
            })
            .unwrap_or(if text == " " { 250.0 } else { 500.0 });
        SimpleGlyph { text, width }
    }))
}

fn embedded_type1_encoding<'a>(bytes: &'a [u8], glyphs: &mut [&'a [u8]; 256]) {
    let end = bytes
        .windows(5)
        .position(|part| part == b"eexec")
        .unwrap_or(bytes.len().min(128 * 1024));
    let mut tokens = bytes[..end]
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|word| !word.is_empty());
    if tokens.by_ref().find(|word| *word == b"/Encoding").is_none() {
        return;
    }
    if tokens.next() == Some(b"StandardEncoding") {
        *glyphs = encodings()[b"StandardEncoding".as_slice()];
    }
    while let Some(token) = tokens.next() {
        if token == b"dup" {
            let code = tokens
                .next()
                .and_then(|word| std::str::from_utf8(word).ok())
                .and_then(|word| word.parse::<usize>().ok());
            let name = tokens.next().and_then(|word| word.strip_prefix(b"/"));
            if let (Some(slot), Some(name)) = (code.and_then(|code| glyphs.get_mut(code)), name) {
                *slot = name;
            }
        } else if token == b"def" {
            break;
        }
    }
}

fn is_math_symbol_font(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "cmsy",
        "cmbsy",
        "lmsy",
        "cmex",
        "lmex",
        "msam",
        "msbm",
        "symbol",
        "mathgreek",
        "mathematicalpi",
        "advmacmthsy",
        "advpsym",
        "advp4c4e",
        "mtex",
        "mtsy",
    ]
    .iter()
    .any(|prefix| lower.contains(prefix))
}

fn fix_decoded_text(text: Cow<'_, str>, math_sym: bool) -> Cow<'_, str> {
    if math_sym && matches!(text.as_ref(), "ffi" | "ffl" | "ff" | "fi" | "fl") {
        // Math/extension fonts (e.g. AdvMacMthSyN / CMEX) sometimes reuse ligature
        // code points for radical extenders or math symbols rather than Latin ligatures.
        return Cow::Borrowed("");
    }
    if let Some(ch) = text.chars().next().filter(|_| text.chars().count() == 1)
        && let Some(code) = u32::from(ch)
            .checked_sub(0xf000)
            .filter(|&code| (0x20..=0xff).contains(&code))
        && let Some(replacement) = glyph_text(encodings()[b"Symbol".as_slice()][code as usize])
    {
        return Cow::Owned(replacement);
    }
    if text
        .chars()
        .any(|ch| matches!(ch, '\u{f8e5}'..='\u{f8fe}' | '\u{f020}'..='\u{f0ff}'))
    {
        return Cow::Owned(normalize(text.chars()));
    }
    text
}

fn tex_encoding(name: &str) -> Option<[&'static [u8]; 256]> {
    let name = name.to_ascii_lowercase();
    if name.starts_with("mathgreek") {
        return Some(encodings()[b"Symbol".as_slice()]);
    }
    if ["cmsy", "cmbsy", "lmsy"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        let mut table = [b".notdef".as_slice(); 256];
        let oms: &[&[u8]] = &[
            b"minus", b"periodcentered", b"multiply", b"asteriskmath", b"divide", b"diamondmath",
            b"plusminus", b"minusplus", b"circleplus", b"circleminus", b"circlemultiply",
            b"circledivide", b"circledot", b"circlecopyrt", b"openbullet", b"bullet",
            b"equivasymptotic", b"equivalence", b"reflexsubset", b"reflexsuperset", b"lessequal",
            b"greaterequal", b"precedesequal", b"followsequal", b"similar", b"approxequal",
            b"propersubset", b"propersuperset", b"lessmuch", b"greatermuch", b"precedes",
            b"follows", b"arrowleft", b"arrowright", b"arrowup", b"arrowdown", b"arrowboth",
            b"arrownortheast", b"arrowsoutheast", b"similarequal", b"arrowdblleft",
            b"arrowdblright", b"arrowdblup", b"arrowdbldown", b"arrowdblboth", b"arrownorthwest",
            b"arrowsouthwest", b"proportional", b"prime", b"infinity", b"element", b"owner",
            b"triangle", b"triangleinv", b"negationslash", b"mapsto", b"universal", b"existential",
            b"logicalnot", b"emptyset", b"Rfraktur", b"Ifraktur", b"latticetop", b"perpendicular",
            b"aleph",
            // 65..=90 are calligraphic A..Z in OMS; keep ASCII letters so formulas stay legible.
            b"A", b"B", b"C", b"D", b"E", b"F", b"G", b"H", b"I", b"J", b"K", b"L", b"M", b"N",
            b"O", b"P", b"Q", b"R", b"S", b"T", b"U", b"V", b"W", b"X", b"Y", b"Z",
            b"union", b"intersection", b"unionmulti", b"logicaland", b"logicalor", b"turnstileleft",
            b"turnstileright", b"floorleft", b"floorright", b"ceilingleft", b"ceilingright",
            b"braceleft", b"braceright", b"angbracketleft", b"angbracketright", b"bar", b"bardbl",
            b"arrowbothv", b"arrowdblbothv", b"backslash", b"wreathproduct", b"radical",
            b"coproduct", b"nabla", b"integral", b"unionsq", b"intersectionsq", b"subsetsqequal",
            b"supersetsqequal", b"section", b"dagger", b"daggerdbl", b"paragraph", b"club",
            b"diamond", b"heart", b"spade",
        ];
        table[..oms.len()].copy_from_slice(oms);
        return Some(table);
    }
    if ["cmex", "lmex"].iter().any(|prefix| name.starts_with(prefix)) {
        let mut table = [b".notdef".as_slice(); 256];
        let omx: &[(usize, &[u8])] = &[
            (0x00, b"parenleftbig"), (0x01, b"parenrightbig"), (0x02, b"bracketleftbig"),
            (0x03, b"bracketrightbig"), (0x04, b"floorleftbig"), (0x05, b"floorrightbig"),
            (0x06, b"ceilingleftbig"), (0x07, b"ceilingrightbig"), (0x08, b"braceleftbig"),
            (0x09, b"bracerightbig"), (0x0a, b"angbracketleftbig"), (0x0b, b"angbracketrightbig"),
            (0x0c, b"vextendsingle"), (0x0d, b"vextenddouble"), (0x0e, b"slashbig"),
            (0x0f, b"backslashbig"), (0x10, b"parenleftBig"), (0x11, b"parenrightBig"),
            (0x12, b"parenleftbigg"), (0x13, b"parenrightbigg"), (0x14, b"bracketleftbigg"),
            (0x15, b"bracketrightbigg"), (0x16, b"floorleftbigg"), (0x17, b"floorrightbigg"),
            (0x18, b"ceilingleftbigg"), (0x19, b"ceilingrightbigg"), (0x1a, b"braceleftbigg"),
            (0x1b, b"bracerightbigg"), (0x1c, b"angbracketleftbigg"), (0x1d, b"angbracketrightbigg"),
            (0x20, b"parenleftBigg"), (0x21, b"parenrightBigg"), (0x22, b"bracketleftBigg"),
            (0x23, b"bracketrightBigg"), (0x24, b"floorleftBigg"), (0x25, b"floorrightBigg"),
            (0x26, b"ceilingleftBigg"), (0x27, b"ceilingrightBigg"), (0x28, b"braceleftBigg"),
            (0x29, b"bracerightBigg"), (0x30, b"parenlefttp"), (0x31, b"parenrighttp"),
            (0x32, b"bracketlefttp"), (0x33, b"bracketrighttp"), (0x34, b"bracketleftbt"),
            (0x35, b"bracketrightbt"), (0x36, b"bracketleftex"), (0x37, b"bracketrightex"),
            (0x38, b"bracelefttp"), (0x39, b"bracerighttp"), (0x3a, b"braceleftbt"),
            (0x3b, b"bracerightbt"), (0x3c, b"braceleftmid"), (0x3d, b"bracerightmid"),
            (0x3e, b"braceex"), (0x40, b"parenleftbt"), (0x41, b"parenrightbt"),
            (0x42, b"parenleftex"), (0x43, b"parenrightex"), (0x48, b"contourintegraltext"),
            (0x49, b"contourintegraldisplay"), (0x4a, b"circledottext"),
            (0x4b, b"circledotdisplay"), (0x4c, b"circleplustext"), (0x4d, b"circleplusdisplay"),
            (0x4e, b"circlemultiplytext"), (0x4f, b"circlemultiplydisplay"),
            (0x50, b"summationtext"), (0x51, b"producttext"), (0x52, b"integraltext"),
            (0x53, b"uniontext"), (0x54, b"intersectiontext"), (0x55, b"unionmultitext"),
            (0x56, b"logicalandtext"), (0x57, b"logicalortext"), (0x58, b"summationdisplay"),
            (0x59, b"productdisplay"), (0x5a, b"integraldisplay"), (0x5b, b"uniondisplay"),
            (0x5c, b"intersectiondisplay"), (0x5d, b"unionmultidisplay"),
            (0x5e, b"logicalanddisplay"), (0x5f, b"logicalordisplay"), (0x60, b"coproducttext"),
            (0x61, b"coproductdisplay"), (0x62, b"hatwide"), (0x63, b"hatwider"),
            (0x64, b"hatwidest"), (0x65, b"tildewide"), (0x66, b"tildewider"),
            (0x67, b"tildewidest"), (0x70, b"radicalbig"), (0x71, b"radicalBig"),
            (0x72, b"radicalbigg"), (0x73, b"radicalBigg"), (0x74, b"radicalbt"),
            (0x75, b"radicalvertex"), (0x76, b"radicaltp"),
        ];
        for &(code, glyph) in omx {
            table[code] = glyph;
        }
        return Some(table);
    }
    if name.starts_with("msbm") {
        let mut table = encodings()[b"StandardEncoding".as_slice()];
        table[0x7e] = b"bar";
        return Some(table);
    }
    let roman = [
        "cmr", "cmb", "cmti", "cmsl", "cmss", "lmr", "lmb", "lmsans", "sfrm", "sfbx", "sfti",
    ];
    let math = ["cmmi", "cmmib", "lmmi"];
    let use_math = math.iter().any(|prefix| name.starts_with(prefix));
    if !use_math && !roman.iter().any(|prefix| name.starts_with(prefix)) {
        return None;
    }
    let mut table = encodings()[b"StandardEncoding".as_slice()];
    let low: &[&[u8]] = if use_math {
        &[
            b"Gamma",
            b"Delta",
            b"Theta",
            b"Lambda",
            b"Xi",
            b"Pi",
            b"Sigma",
            b"Upsilon",
            b"Phi",
            b"Psi",
            b"Omega",
            b"alpha",
            b"beta",
            b"gamma",
            b"delta",
            b"epsilon",
            b"zeta",
            b"eta",
            b"theta",
            b"iota",
            b"kappa",
            b"lambda",
            b"mu",
            b"nu",
            b"xi",
            b"pi",
            b"rho",
            b"sigma",
            b"tau",
            b"upsilon",
            b"phi",
            b"chi",
            b"psi",
            b"omega",
            b"varepsilon",
            b"vartheta",
            b"varpi",
            b"varrho",
            b"varsigma",
            b"varphi",
        ]
    } else {
        &[
            b"Gamma",
            b"Delta",
            b"Theta",
            b"Lambda",
            b"Xi",
            b"Pi",
            b"Sigma",
            b"Upsilon",
            b"Phi",
            b"Psi",
            b"Omega",
            b"ff",
            b"fi",
            b"fl",
            b"ffi",
            b"ffl",
            b"dotlessi",
            b"dotlessj",
            b"grave",
            b"acute",
            b"caron",
            b"breve",
            b"macron",
            b"ring",
            b"cedilla",
            b"germandbls",
            b"ae",
            b"oe",
            b"oslash",
            b"AE",
            b"OE",
            b"Oslash",
        ]
    };
    table[..low.len()].copy_from_slice(low);
    if !use_math {
        table[92] = b"quotedblleft";
        table[94] = b"circumflex";
        table[95] = b"dotaccent";
        table[123] = b"endash";
        table[124] = b"emdash";
        table[125] = b"hungarumlaut";
        table[126] = b"tilde";
        table[127] = b"dieresis";
    }
    Some(table)
}

fn remove_subset_prefix(name: &[u8]) -> &[u8] {
    if name.get(6) == Some(&b'+') && name[..6].iter().all(u8::is_ascii_uppercase) {
        &name[7..]
    } else {
        name
    }
}

fn standard_name(name: &str) -> Cow<'_, str> {
    let lower = name.to_ascii_lowercase();
    if lower == "symbolmt" {
        return "Symbol".into();
    }
    if let Some(family) = [
        ("arial", "Helvetica"),
        ("timesnewroman", "Times"),
        ("couriernew", "Courier"),
    ]
    .into_iter()
    .find_map(|(alias, family)| lower.starts_with(alias).then_some(family))
    {
        let bold = lower.contains("bold");
        let italic = lower.contains("italic") || lower.contains("oblique");
        let suffix = match (family, bold, italic) {
            ("Times", false, false) => "-Roman",
            ("Times", false, true) => "-Italic",
            ("Times", true, true) => "-BoldItalic",
            (_, false, false) => "",
            (_, true, false) => "-Bold",
            (_, false, true) => "-Oblique",
            (_, true, true) => "-BoldOblique",
        };
        return format!("{family}{suffix}").into();
    }
    name.into()
}

fn glyph_text(name: &[u8]) -> Option<String> {
    static AGL: OnceLock<HashMap<&'static [u8], &'static str>> = OnceLock::new();
    let agl = AGL.get_or_init(|| {
        [
            include_str!("../data/glyphlist.txt"),
            include_str!("../data/zapfdingbats.txt"),
        ]
        .into_iter()
        .flat_map(str::lines)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once(';'))
        .map(|(name, codes)| (name.as_bytes(), codes))
        .collect()
    });
    let name = name.split(|&byte| byte == b'.').next()?;
    let mut text = String::new();
    for part in name.split(|&byte| byte == b'_') {
        let special = match part {
            b"Delta" => "Δ",
            b"Omega" => "Ω",
            b"mu" => "μ",
            b"epsilon1" | b"varepsilon" => "ϵ",
            b"theta1" | b"vartheta" => "ϑ",
            b"phi1" | b"varphi" => "ϕ",
            b"omega1" | b"varpi" => "ϖ",
            b"varrho" => "ϱ",
            b"varsigma" => "ς",
            b"dotlessj" => "ȷ",
            b"negationslash" => "̸",
            b"mapsto" => "↦",
            b"coproduct" | b"coproducttext" | b"coproductdisplay" => "∐",
            b"unionmulti" | b"unionmultitext" | b"unionmultidisplay" => "⊎",
            b"unionsq" => "⊔",
            b"intersectionsq" => "⊓",
            b"subsetsqequal" => "⊑",
            b"supersetsqequal" => "⊒",
            b"angbracketleft" | b"angbracketleftbig" | b"angbracketleftBig"
            | b"angbracketleftbigg" | b"angbracketleftBigg" => "⟨",
            b"angbracketright" | b"angbracketrightbig" | b"angbracketrightBig"
            | b"angbracketrightbigg" | b"angbracketrightBigg" => "⟩",
            b"floorleft" | b"floorleftbig" | b"floorleftBig" | b"floorleftbigg"
            | b"floorleftBigg" => "⌊",
            b"floorright" | b"floorrightbig" | b"floorrightBig" | b"floorrightbigg"
            | b"floorrightBigg" => "⌋",
            b"ceilingleft" | b"ceilingleftbig" | b"ceilingleftBig" | b"ceilingleftbigg"
            | b"ceilingleftBigg" => "⌈",
            b"ceilingright" | b"ceilingrightbig" | b"ceilingrightBig" | b"ceilingrightbigg"
            | b"ceilingrightBigg" => "⌉",
            b"asteriskmath" => "∗",
            b"minusplus" => "∓",
            b"diamondmath" => "⋄",
            b"circleminus" => "⊖",
            b"circledivide" => "⊘",
            b"circledot" | b"circledottext" | b"circledotdisplay" => "⊙",
            b"circleplustext" | b"circleplusdisplay" => "⊕",
            b"circlemultiplytext" | b"circlemultiplydisplay" => "⊗",
            b"openbullet" => "∘",
            b"equivasymptotic" => "≍",
            b"precedes" => "≺",
            b"follows" => "≻",
            b"precedesequal" => "≼",
            b"followsequal" => "≽",
            b"similarequal" => "≃",
            b"arrownortheast" => "↗",
            b"arrowsoutheast" => "↘",
            b"arrownorthwest" => "↖",
            b"arrowsouthwest" => "↙",
            b"arrowbothv" => "↕",
            b"arrowdblbothv" => "⇕",
            b"owner" => "∋",
            b"triangle" => "△",
            b"triangleinv" => "▽",
            b"turnstileleft" => "⊢",
            b"turnstileright" => "⊣",
            b"wreathproduct" => "≀",
            b"hacek" => "ˇ",
            b"tie" => "⁀",
            b"prime" => "′",
            b"star" => "⋆",
            b"vector" => "⃗",
            b"lscript" => "ℓ",
            b"circlecopyrt" => "©",
            b"greatermuch" => "≫",
            b"lessmuch" => "≪",
            b"latticetop" => "⊤",
            b"latticebottom" => "⊥",
            b"suppress" => "⊘",
            b"bardbl" | b"vextenddouble" => "‖",
            b"vextendsingle" => "|",
            b"radicalbig" | b"radicalBig" | b"radicalbigg" | b"radicalBigg" | b"radicalbt"
            | b"radicaltp" | b"radicalvertex" => "√",
            b"integraltext" | b"integraldisplay" => "∫",
            b"contourintegraltext" | b"contourintegraldisplay" => "∮",
            b"summationtext" | b"summationdisplay" => "∑",
            b"producttext" | b"productdisplay" => "∏",
            b"uniontext" | b"uniondisplay" => "⋃",
            b"intersectiontext" | b"intersectiondisplay" => "⋂",
            b"logicalandtext" | b"logicalanddisplay" => "⋀",
            b"logicalortext" | b"logicalordisplay" => "⋁",
            b"hatwide" | b"hatwider" | b"hatwidest" => "^",
            b"tildewide" | b"tildewider" | b"tildewidest" => "~",
            b"slashbig" | b"slashBig" | b"slashbigg" | b"slashBigg" => "/",
            b"backslashbig" | b"backslashBig" | b"backslashbigg" | b"backslashBigg" => "\\",
            b"parenleftbig" | b"parenleftBig" | b"parenleftbigg" | b"parenleftBigg"
            | b"parenlefttp" | b"parenleftbt" => "(",
            b"parenrightbig" | b"parenrightBig" | b"parenrightbigg" | b"parenrightBigg"
            | b"parenrighttp" | b"parenrightbt" => ")",
            b"bracketleftbig" | b"bracketleftBig" | b"bracketleftbigg" | b"bracketleftBigg"
            | b"bracketlefttp" | b"bracketleftbt" => "[",
            b"bracketrightbig" | b"bracketrightBig" | b"bracketrightbigg"
            | b"bracketrightBigg" | b"bracketrighttp" | b"bracketrightbt" => "]",
            b"braceleftbig" | b"braceleftBig" | b"braceleftbigg" | b"braceleftBigg"
            | b"bracelefttp" | b"braceleftmid" | b"braceleftbt" => "{",
            b"bracerightbig" | b"bracerightBig" | b"bracerightbigg" | b"bracerightBigg"
            | b"bracerighttp" | b"bracerightmid" | b"bracerightbt" => "}",
            b"parenleftex" | b"parenrightex" | b"bracketleftex" | b"bracketrightex"
            | b"braceex" => "|",
            b"nbspace" | b"visiblespace" | b"nonbreakingspace" => " ",
            b"hyphenchar" | b"sfthyphen" => "-",
            _ => "",
        };
        if !special.is_empty() {
            text.push_str(special);
        } else if let Some(codes) = agl.get(part) {
            for code in codes.split_ascii_whitespace() {
                text.push(char::from_u32(u32::from_str_radix(code, 16).ok()?)?);
            }
        } else if let Some(codes) = part
            .strip_prefix(b"uni")
            .filter(|codes| !codes.is_empty() && codes.len() % 4 == 0)
        {
            let units =
                codes.as_chunks::<4>().0.iter().map(|digits| {
                    u16::from_str_radix(std::str::from_utf8(digits).unwrap_or(""), 16)
                });
            let units: Vec<_> = units.collect::<Result<_, _>>().ok()?;
            text.push_str(&String::from_utf16(&units).ok()?);
        } else if let Some(code) = part
            .strip_prefix(b"u")
            .filter(|code| (4..=6).contains(&code.len()))
        {
            text.push(char::from_u32(
                u32::from_str_radix(std::str::from_utf8(code).ok()?, 16).ok()?,
            )?);
        } else {
            let base = part
                .strip_suffix(b"small")
                .filter(|base| !base.is_empty() && base.len() <= 32)?;
            text.push_str(&glyph_text(base)?);
        }
    }
    Some(normalize(text.chars()))
}

pub(crate) fn normalize(chars: impl IntoIterator<Item = char>) -> String {
    let mut text = String::new();
    for character in chars {
        match character {
            'ﬁ' => text.push_str("fi"),
            'ﬂ' => text.push_str("fl"),
            'ﬀ' => text.push_str("ff"),
            'ﬃ' => text.push_str("ffi"),
            'ﬄ' => text.push_str("ffl"),
            'ﬅ' | 'ﬆ' => text.push_str("st"),
            '\u{a0}' | '\u{202f}' | '\t' | '\r' | '\n' => text.push(' '),
            // Adobe Corporate Use Area (PUA) math delimiter/extender pieces from AGL
            '\u{f8e5}' => text.push('‾'),
            '\u{f8e6}' => text.push('|'),
            '\u{f8e7}' => text.push('─'),
            '\u{f8eb}' | '\u{f8ed}' => text.push('('),
            '\u{f8ec}' | '\u{f8ef}' | '\u{f8f4}' | '\u{f8f7}' | '\u{f8fa}' => text.push('|'),
            '\u{f8ee}' | '\u{f8f0}' => text.push('['),
            '\u{f8f1}' | '\u{f8f2}' | '\u{f8f3}' => text.push('{'),
            '\u{f8f5}' => text.push('∫'),
            '\u{f8f6}' | '\u{f8f8}' => text.push(')'),
            '\u{f8f9}' | '\u{f8fb}' => text.push(']'),
            '\u{f8fc}' | '\u{f8fd}' | '\u{f8fe}' => text.push('}'),
            // Microsoft Symbol PUA block (0xF020..=0xF0FF)
            '\u{f020}'..='\u{f0ff}' => {
                let code = (u32::from(character) - 0xf000) as usize;
                if let Some(replacement) = glyph_text(encodings()[b"Symbol".as_slice()][code]) {
                    text.push_str(&replacement);
                } else {
                    text.push(character);
                }
            }
            ch if ch.is_control() => text.push(char::REPLACEMENT_CHARACTER),
            ch => text.push(ch),
        }
    }
    text
}

type Encodings = HashMap<&'static [u8], [&'static [u8]; 256]>;

fn encodings() -> &'static Encodings {
    static ENCODINGS: OnceLock<Encodings> = OnceLock::new();
    ENCODINGS.get_or_init(|| {
        include_str!("../data/encodings.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| {
                let (name, entries) = line.split_once(' ')?;
                let mut entries = entries.split_ascii_whitespace().map(str::as_bytes);
                Some((
                    name.as_bytes(),
                    std::array::from_fn(|_| entries.next().unwrap_or(b"-")),
                ))
            })
            .collect()
    })
}

struct StandardMetrics {
    ascent: f64,
    descent: f64,
    widths: HashMap<&'static str, f64>,
    codes: [&'static str; 256],
}

fn standard_fonts() -> &'static HashMap<&'static str, StandardMetrics> {
    static FONTS: OnceLock<HashMap<&'static str, StandardMetrics>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = HashMap::new();
        let mut current = None;
        for line in include_str!("../data/standard-font-metrics.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let mut fields = line.split_ascii_whitespace();
            match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some("@"), Some(name), Some(ascent), Some(descent)) => {
                    current = Some(&mut *fonts.entry(name).or_insert_with(|| StandardMetrics {
                        ascent: ascent.parse().expect("AFM ascent"),
                        descent: descent.parse().expect("AFM descent"),
                        widths: HashMap::new(),
                        codes: [".notdef"; 256],
                    }));
                }
                (Some(glyph), Some(width), Some(code), None) => {
                    if let Some(current) = current.as_deref_mut() {
                        current
                            .widths
                            .insert(glyph, width.parse().expect("AFM width"));
                        if let Ok(code) = code.parse::<u8>() {
                            current.codes[usize::from(code)] = glyph;
                        }
                    }
                }
                _ => {}
            }
        }
        fonts
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    #[test]
    fn adobe_and_tex_names() {
        for (name, text) in [
            ("Aogonek", "Ą"),
            ("dalethatafpatah", "דֲ"),
            ("dieresis", "¨"),
            ("uni00410301", "A\u{301}"),
            ("u1D49C.alt", "𝒜"),
            ("f_f_i", "ffi"),
            ("lscript", "ℓ"),
            ("varepsilon", "ϵ"),
            ("parenleftBigg", "("),
            ("a1", "✁"),
        ] {
            assert_eq!(glyph_text(name.as_bytes()).as_deref(), Some(text), "{name}");
        }
        assert_eq!(glyph_text(b".notdef"), None);
    }

    #[test]
    fn ligatures_and_nonbreaking_spaces() {
        assert_eq!(
            normalize("ﬁﬂﬀﬃﬄ\u{a0}a\u{202f}b".chars()),
            "fiflffffiffl a b"
        );
    }

    #[test]
    fn win_ansi_with_differences_and_standard_widths() {
        let font = Font::new(
            &Document::new(),
            &dictionary! {
                "Subtype" => "Type1",
                "BaseFont" => "ABCXYZ+Helvetica-BoldOblique",
                "Encoding" => dictionary! {
                    "BaseEncoding" => "WinAnsiEncoding",
                    "Differences" => vec![Object::Integer(65), Object::Name(b"ffi".to_vec()), Object::Name(b"u1F642".to_vec())],
                },
            },
            b"F1",
        );
        assert_eq!(font.name, "Helvetica-BoldOblique");
        assert!(font.bold && font.italic);
        assert_eq!(font.decode(b"A").text, "ffi");
        assert_eq!(font.decode(b"B").text, "🙂");
        assert_eq!(font.decode(&[0x80]).text, "€");
        assert_eq!(font.decode(&[0x93]).text, "“");
        assert_eq!(font.decode(&[0xa0]).text, " ");
        assert_eq!(font.decode(b" ").width, 278.0);
    }

    #[test]
    fn cid_width_array_and_interval() {
        let font = Font::new(
            &Document::new(),
            &dictionary! {
                "Subtype" => "Type0", "BaseFont" => "CID", "Encoding" => "Identity-V",
                "DescendantFonts" => vec![Object::Dictionary(dictionary! {
                    "DW" => 610,
                    "DW2" => vec![800.into(), (-1000).into()],
                    "W" => vec![65.into(), vec![700.into(), 720.into()].into(), 72.into(), 90.into(), 500.into()],
                    "W2" => vec![65.into(), 66.into(), (-900).into(), 350.into(), 850.into()],
                })]
            },
            b"F1",
        );
        assert!(font.vertical);
        assert_eq!(font.decode(b"\0A").width, 700.0);
        assert_eq!(font.decode(b"\0B").width, 720.0);
        assert_eq!(font.decode(b"\0C").width, 610.0);
        assert_eq!(font.decode(b"\0H").width, 500.0);
        assert_eq!(font.decode(b"\0B").vertical_metric, [-900.0, 350.0, 850.0]);
        assert_eq!(font.decode(b"\0C").vertical_metric, [-1000.0, 305.0, 800.0]);
    }

    #[test]
    fn pua_and_tex_math_fonts() {
        assert_eq!(normalize("\u{f8eb}\u{f8f6}\u{f8ee}\u{f8f9}".chars()), "()[]");
        assert_eq!(fix_decoded_text(Cow::Borrowed("\u{f061}"), false), "α");
        assert_eq!(fix_decoded_text(Cow::Borrowed("ffi"), true), "");
        assert_eq!(fix_decoded_text(Cow::Borrowed("ffi"), false), "ffi");
        assert!(is_math_symbol_font("AdvP4C4E46"));
        assert!(is_math_symbol_font("AdvMacMthSyN"));
        assert!(tex_encoding("CMSY10").is_some());
        assert!(tex_encoding("CMEX10").is_some());
        assert_eq!(glyph_text(b"approxequal").as_deref(), Some("≈"));
        assert_eq!(glyph_text(b"summationdisplay").as_deref(), Some("∑"));
    }
}

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, content::Content};

use crate::{
    BoundingBox, GraphicKind, GraphicObject, LayoutDocument, PageInfo, PdfError, PdfLink,
    PdfMetadata, PdfOutline,
    fonts::{Color, Font},
    layout,
};

const CONTENT_LIMIT: usize = 64 * 1024 * 1024;
const FORM_DEPTH_LIMIT: usize = 24;

pub(crate) fn resolve<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object {
        Object::Reference(id) => doc.get_object(*id).ok(),
        object => Some(object),
    }
}

pub(crate) fn entry<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    resolve(doc, dict.get(key).ok()?)
}

pub(crate) fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) if value.is_finite() => Some(f64::from(*value)),
        _ => None,
    }
}

fn numbers<const N: usize>(objects: &[Object]) -> Option<[f64; N]> {
    if objects.len() < N {
        return None;
    }
    let mut values = [0.0; N];
    for (value, object) in values.iter_mut().zip(objects) {
        *value = number(object)?;
    }
    Some(values)
}

pub(crate) fn stream_bytes<'a>(
    doc: &'a Document,
    stream: &'a Stream,
) -> lopdf::Result<Cow<'a, [u8]>> {
    let Some(filter) =
        entry(doc, &stream.dict, b"Filter").filter(|object| !matches!(object, Object::Null))
    else {
        if stream.content.len() > CONTENT_LIMIT {
            return Err(lopdf::DecompressError::MemoryLimitExceeded {
                limit: CONTENT_LIMIT,
            }
            .into());
        }
        return Ok(stream.content.as_slice().into());
    };
    let canonical = |name: &[u8]| -> Option<&'static [u8]> {
        match name {
            b"Fl" => Some(b"FlateDecode"),
            b"LZW" => Some(b"LZWDecode"),
            b"A85" => Some(b"ASCII85Decode"),
            b"AHx" => Some(b"ASCIIHexDecode"),
            b"RL" => Some(b"RunLengthDecode"),
            _ => None,
        }
    };
    let decode_params = stream.dict.get(b"DecodeParms").ok();
    let simple_filter =
        |filter: &Object| filter.as_name().is_ok_and(|name| canonical(name).is_none());
    let indirect_params = match decode_params {
        Some(Object::Reference(_)) => true,
        Some(Object::Array(items)) => items
            .iter()
            .any(|item| matches!(item, Object::Reference(_))),
        _ => false,
    };
    if !indirect_params
        && stream.dict.get(b"Filter").is_ok_and(|original| {
            simple_filter(original)
                || original
                    .as_array()
                    .is_ok_and(|filters| !filters.is_empty() && filters.iter().all(simple_filter))
        })
    {
        return stream
            .decompressed_content_with_limit(CONTENT_LIMIT)
            .map(Cow::Owned);
    }

    let mut adapted = stream.clone();
    let filter_name = |object: &Object| {
        let name = resolve(doc, object)
            .and_then(|obj| obj.as_name().ok())
            .ok_or(lopdf::Error::Unimplemented("invalid stream filter"))?;
        Ok::<_, lopdf::Error>(Object::Name(canonical(name).unwrap_or(name).to_vec()))
    };
    if let Ok(filters) = filter.as_array() {
        if filters.is_empty() {
            if stream.content.len() > CONTENT_LIMIT {
                return Err(lopdf::DecompressError::MemoryLimitExceeded {
                    limit: CONTENT_LIMIT,
                }
                .into());
            }
            return Ok(stream.content.as_slice().into());
        }
        adapted.dict.set(
            "Filter",
            Object::Array(filters.iter().map(filter_name).collect::<Result<_, _>>()?),
        );
    } else {
        adapted.dict.set("Filter", filter_name(filter)?);
    }
    if indirect_params && let Some(params) = entry(doc, &stream.dict, b"DecodeParms") {
        let params = if let Ok(array) = params.as_array() {
            Object::Array(
                array
                    .iter()
                    .map(|param| resolve(doc, param).cloned().unwrap_or(Object::Null))
                    .collect(),
            )
        } else {
            params.clone()
        };
        adapted.dict.set("DecodeParms", params);
    }
    adapted
        .decompressed_content_with_limit(CONTENT_LIMIT)
        .map(Cow::Owned)
}

fn inherited<'a>(doc: &'a Document, mut node: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    for _ in 0..128 {
        if let Some(value) = entry(doc, node, key).filter(|obj| !matches!(obj, Object::Null)) {
            return Some(value);
        }
        node = entry(doc, node, b"Parent")?.as_dict().ok()?;
    }
    None
}

pub(crate) fn extract(
    doc: &Document,
    options: crate::ExtractOptions,
) -> Result<LayoutDocument, PdfError> {
    let metadata = entry(doc, &doc.trailer, b"Info").and_then(|object| object.as_dict().ok());
    let field = |key: &[u8]| {
        let value = entry(doc, metadata?, key)?;
        lopdf::decode_text_string(value)
            .ok()
            .filter(|text| !text.trim().is_empty())
    };
    let mut output = LayoutDocument {
        metadata: PdfMetadata {
            title: field(b"Title"),
            author: field(b"Author"),
            subject: field(b"Subject"),
            keywords: field(b"Keywords"),
            creator: field(b"Creator"),
            producer: field(b"Producer"),
            doi: field(b"doi")
                .or_else(|| field(b"DOI"))
                .or_else(|| field(b"WPS-ARTICLEDOI")),
            isbn: field(b"ISBN"),
            issn: field(b"ISSN"),
            arxiv: field(b"arXiv"),
            creation_date: field(b"CreationDate"),
            modified_date: field(b"ModDate"),
            xmp: entry(doc, &doc.trailer, b"Root")
                .and_then(|root| entry(doc, root.as_dict().ok()?, b"Metadata"))
                .and_then(|metadata| stream_bytes(doc, metadata.as_stream().ok()?).ok())
                .map(|xml| crate::metadata::parse_xmp(&xml))
                .unwrap_or_default(),
        },
        ..LayoutDocument::default()
    };
    let mut interpreter = Interpreter {
        doc,
        fonts: Vec::new(),
        font_ids: HashMap::new(),
        glyphs: Vec::new(),
        graphics: Vec::new(),
        active_forms: Vec::new(),
        remaining: CONTENT_LIMIT,
    };
    let mut page_numbers: HashMap<ObjectId, usize> = HashMap::new();
    let mut first_error = None;
    for (page, id) in doc
        .page_iter()
        .take(options.max_pages.unwrap_or(usize::MAX))
        .enumerate()
    {
        let page = page + 1;
        page_numbers.insert(id, page);
        let error = |message| PdfError::Page { page, message };
        let links_start = output.links.len();
        let result = (|| -> Result<(), PdfError> {
            let node = doc
                .get_dictionary(id)
                .map_err(|cause| error(cause.to_string()))?;
            let rectangle = |key: &[u8]| {
                let array = inherited(doc, node, key)?.as_array().ok()?;
                let [x0, y0, x1, y1] = numbers(array)?;
                Some(BoundingBox::new(
                    page,
                    x0.min(x1),
                    y0.min(y1),
                    (x1 - x0).abs(),
                    (y1 - y0).abs(),
                ))
            };
            let media = rectangle(b"MediaBox");
            let crop = match (media, rectangle(b"CropBox")) {
                (Some(media), Some(crop)) => media.intersection(&crop, false).or(Some(media)),
                (media, crop) => crop.or(media),
            }
            .filter(|rect| rect.width > 0.0 && rect.height > 0.0)
            .ok_or_else(|| error("missing or empty page box".into()))?;
            let rotate = inherited(doc, node, b"Rotate")
                .and_then(|obj| obj.as_i64().ok())
                .unwrap_or(0)
                .rem_euclid(360);
            let unit = entry(doc, node, b"UserUnit")
                .and_then(number)
                .filter(|&value| value > 0.0 && value <= 75000.0)
                .unwrap_or(1.0);
            let transform = Matrix::scale(unit, unit).then(match rotate {
                90 => Matrix([0.0, 1.0, 1.0, 0.0, -crop.y, -crop.x]),
                180 => Matrix([-1.0, 0.0, 0.0, 1.0, crop.x2(), -crop.y]),
                270 => Matrix([0.0, -1.0, -1.0, 0.0, crop.y2(), crop.x2()]),
                _ => Matrix([1.0, 0.0, 0.0, -1.0, -crop.x, crop.y2()]),
            });
            let (width, height) = if matches!(rotate, 90 | 270) {
                (crop.height * unit, crop.width * unit)
            } else {
                (crop.width * unit, crop.height * unit)
            };
            let page_box = BoundingBox::new(page, 0.0, 0.0, width, height);
            extract_page_links(doc, node, page, transform, &mut output.links);
            let resources = inherited(doc, node, b"Resources").and_then(|obj| obj.as_dict().ok());
            interpreter.remaining = CONTENT_LIMIT;
            if let Some(contents) = entry(doc, node, b"Contents") {
                let data: Cow<'_, [u8]> = if let Ok(array) = contents.as_array() {
                    let mut data = Vec::new();
                    for stream in array
                        .iter()
                        .filter_map(|obj| resolve(doc, obj)?.as_stream().ok())
                    {
                        let bytes =
                            stream_bytes(doc, stream).map_err(|cause| error(cause.to_string()))?;
                        if bytes.len() >= CONTENT_LIMIT.saturating_sub(data.len()) {
                            return Err(error("page content exceeds decode limit".into()));
                        }
                        data.extend_from_slice(&bytes);
                        data.push(b'\n');
                    }
                    Cow::Owned(data)
                } else if let Ok(stream) = contents.as_stream() {
                    stream_bytes(doc, stream).map_err(|cause| error(cause.to_string()))?
                } else {
                    Cow::Borrowed(&[])
                };
                interpreter
                    .run(
                        &data,
                        resources,
                        State {
                            ctm: transform,
                            clip: page_box,
                            ..State::default()
                        },
                    )
                    .map_err(error)?;
            }
            let orcid_boxes: Vec<BoundingBox> = output
                .links
                .iter()
                .filter(|l| l.page == page && l.uri.to_ascii_lowercase().contains("orcid.org/"))
                .map(|l| l.bbox)
                .collect();
            if !orcid_boxes.is_empty() {
                interpreter.glyphs.retain(|g| {
                    let text = g.text.as_str();
                    if text.is_empty() || !text.chars().all(|c| matches!(c, ']' | '[')) {
                        return true;
                    }
                    let cx = g.bbox.x + g.bbox.width / 2.0;
                    let cy = g.bbox.y + g.bbox.height / 2.0;
                    !orcid_boxes.iter().any(|b| {
                        cx >= b.x - 2.0
                            && cx <= b.x2() + 2.0
                            && cy >= b.y - 2.0
                            && cy <= b.y2() + 2.0
                    })
                });
            }
            let mut info = PageInfo {
                number: page,
                width,
                height,
                main_area: page_box,
                block_range: output.blocks.len()..output.blocks.len(),
                line_range: output.lines.len()..output.lines.len(),
                token_range: output.tokens.len()..output.tokens.len(),
            };
            layout::append_page(
                &mut output,
                &mut info,
                &mut interpreter.glyphs,
                &interpreter.fonts,
                &interpreter.graphics,
            );
            output.graphics.append(&mut interpreter.graphics);
            output.pages.push(info);
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("Skipping PDF page: {error}");
            first_error.get_or_insert(error);
            interpreter.glyphs.clear();
            interpreter.graphics.clear();
            interpreter.active_forms.clear();
            output.links.truncate(links_start);
        }
    }
    if output.pages.is_empty()
        && let Some(error) = first_error
    {
        return Err(error);
    }
    if options.max_pages.is_none() {
        extract_outlines(doc, &page_numbers, &mut output.outlines);
    }
    Ok(output)
}

fn extract_page_links(
    doc: &Document,
    page_node: &Dictionary,
    page: usize,
    transform: Matrix,
    out: &mut Vec<PdfLink>,
) {
    let Some(annots) = entry(doc, page_node, b"Annots").and_then(|obj| obj.as_array().ok()) else {
        return;
    };
    for annot in annots
        .iter()
        .filter_map(|obj| resolve(doc, obj)?.as_dict().ok())
    {
        if entry(doc, annot, b"Subtype").and_then(|obj| obj.as_name().ok()) != Some(b"Link") {
            continue;
        }
        let Some(action) = entry(doc, annot, b"A").and_then(|obj| obj.as_dict().ok()) else {
            continue;
        };
        if entry(doc, action, b"S").and_then(|obj| obj.as_name().ok()) != Some(b"URI") {
            continue;
        }
        let Some(uri_obj) = entry(doc, action, b"URI") else {
            continue;
        };
        let uri = lopdf::decode_text_string(uri_obj)
            .ok()
            .or_else(|| {
                uri_obj
                    .as_str()
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            })
            .map(|text| text.trim().to_owned())
            .unwrap_or_default();
        if uri.is_empty() {
            continue;
        }
        let bbox = entry(doc, annot, b"Rect")
            .and_then(|obj| obj.as_array().ok())
            .and_then(|array| numbers::<4>(array))
            .map(|[x0, y0, x1, y1]| transform.rect(page, x0, y0, x1, y1))
            .filter(|rect| rect.x.is_finite() && rect.y.is_finite())
            .unwrap_or_else(|| BoundingBox::new(page, 0.0, 0.0, 0.0, 0.0));
        out.push(PdfLink { page, bbox, uri });
    }
}

fn extract_outlines(
    doc: &Document,
    page_numbers: &HashMap<ObjectId, usize>,
    out: &mut Vec<PdfOutline>,
) {
    let Some(root) = entry(doc, &doc.trailer, b"Root").and_then(|obj| obj.as_dict().ok()) else {
        return;
    };
    let Some(outlines) = entry(doc, root, b"Outlines").and_then(|obj| obj.as_dict().ok()) else {
        return;
    };
    let Some(first) = outlines
        .get(b"First")
        .ok()
        .and_then(|obj| obj.as_reference().ok())
    else {
        return;
    };
    let mut visited = HashSet::new();
    walk_outlines(doc, first, 0, page_numbers, &mut visited, out);
}

fn walk_outlines(
    doc: &Document,
    mut current: ObjectId,
    level: usize,
    page_numbers: &HashMap<ObjectId, usize>,
    visited: &mut HashSet<ObjectId>,
    out: &mut Vec<PdfOutline>,
) {
    if level >= 8 || out.len() >= 512 {
        return;
    }
    while out.len() < 512 && visited.insert(current) {
        let Ok(dict) = doc.get_dictionary(current) else {
            break;
        };
        if let Some(title) = entry(doc, dict, b"Title")
            .and_then(|obj| lopdf::decode_text_string(obj).ok())
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|text| !text.is_empty())
        {
            let page = outline_page(doc, dict, page_numbers);
            out.push(PdfOutline { title, page, level });
        }
        if let Ok(child) = dict.get(b"First").and_then(Object::as_reference) {
            walk_outlines(doc, child, level + 1, page_numbers, visited, out);
        }
        let Ok(next) = dict.get(b"Next").and_then(Object::as_reference) else {
            break;
        };
        current = next;
    }
}

fn outline_page(
    doc: &Document,
    dict: &Dictionary,
    page_numbers: &HashMap<ObjectId, usize>,
) -> Option<usize> {
    let dest = entry(doc, dict, b"Dest").or_else(|| {
        let action = entry(doc, dict, b"A")?.as_dict().ok()?;
        (entry(doc, action, b"S")?.as_name().ok()? == b"GoTo")
            .then(|| entry(doc, action, b"D"))
            .flatten()
    })?;
    let array = dest.as_array().ok()?;
    let page_ref = array.first()?.as_reference().ok()?;
    page_numbers.get(&page_ref).copied()
}

#[derive(Clone, Copy, Debug)]
struct Matrix([f64; 6]);

impl Matrix {
    const IDENTITY: Self = Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    fn translation(x: f64, y: f64) -> Self {
        Self([1.0, 0.0, 0.0, 1.0, x, y])
    }

    fn scale(x: f64, y: f64) -> Self {
        Self([x, 0.0, 0.0, y, 0.0, 0.0])
    }

    // Apply the local transform, then this transform.
    fn then(self, local: Self) -> Self {
        let [a, b, c, d, e, f] = self.0;
        let [u, v, w, x, y, z] = local.0;
        Self([
            a * u + c * v,
            b * u + d * v,
            a * w + c * x,
            b * w + d * x,
            a * y + c * z + e,
            b * y + d * z + f,
        ])
    }

    fn point(self, x: f64, y: f64) -> Point {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    fn rect(self, page: usize, x0: f64, y0: f64, x1: f64, y1: f64) -> BoundingBox {
        let corners = [
            self.point(x0, y0),
            self.point(x1, y0),
            self.point(x1, y1),
            self.point(x0, y1),
        ];
        bounds(page, &corners)
            .unwrap_or_else(|| BoundingBox::new(page, f64::NAN, f64::NAN, 0.0, 0.0))
    }
}

type Point = (f64, f64);

fn bounds(page: usize, points: &[Point]) -> Option<BoundingBox> {
    let &(mut x0, mut y0) = points.first()?;
    let (mut x1, mut y1) = (x0, y0);
    for &(x, y) in points {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    Some(BoundingBox::new(page, x0, y0, x1 - x0, y1 - y0))
}

#[derive(Clone, Copy)]
struct TextState {
    font: Option<usize>,
    size: f64,
    char_space: f64,
    word_space: f64,
    hscale: f64,
    leading: f64,
    rise: f64,
    render: i64,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            font: None,
            size: 0.0,
            char_space: 0.0,
            word_space: 0.0,
            hscale: 1.0,
            leading: 0.0,
            rise: 0.0,
            render: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct Position {
    text: Matrix,
    line: Matrix,
    active: bool,
}

impl Default for Position {
    fn default() -> Self {
        Self {
            text: Matrix::IDENTITY,
            line: Matrix::IDENTITY,
            active: false,
        }
    }
}

impl Position {
    fn move_line(&mut self, x: f64, y: f64) {
        self.line = self.line.then(Matrix::translation(x, y));
        self.text = self.line;
    }

    fn advance(&mut self, x: f64, y: f64) {
        self.text = self.text.then(Matrix::translation(x, y));
    }
}

#[derive(Clone, Copy)]
struct State {
    ctm: Matrix,
    clip: BoundingBox,
    fill: Color,
    stroke: Color,
    fill_components: usize,
    stroke_components: usize,
    line_width: f64,
    text: TextState,
}

impl Default for State {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            clip: BoundingBox::new(1, 0.0, 0.0, 0.0, 0.0),
            fill: (0, 0, 0),
            stroke: (0, 0, 0),
            fill_components: 1,
            stroke_components: 1,
            line_width: 1.0,
            text: TextState::default(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Direction {
    Right,
    Down,
    Left,
    Up,
    Angled(i16),
}

impl Direction {
    fn from_vector(dx: f64, dy: f64) -> Self {
        const AXIS_TOLERANCE: f64 = 0.025;
        if dy.abs() <= AXIS_TOLERANCE * dx.abs() {
            if dx >= 0.0 { Self::Right } else { Self::Left }
        } else if dx.abs() <= AXIS_TOLERANCE * dy.abs() {
            if dy >= 0.0 { Self::Down } else { Self::Up }
        } else {
            // Quarter-degree buckets absorb the rounding typical of PDF matrices.
            Self::Angled((dy.atan2(dx).to_degrees() * 4.0).round() as i16)
        }
    }

    fn axis(self) -> (f64, f64) {
        match self {
            Self::Right => (1.0, 0.0),
            Self::Down => (0.0, 1.0),
            Self::Left => (-1.0, 0.0),
            Self::Up => (0.0, -1.0),
            Self::Angled(angle) => {
                let (sine, cosine) = (f64::from(angle) / 4.0).to_radians().sin_cos();
                (cosine, sine)
            }
        }
    }

    pub(crate) fn fragment_axis(self) -> (bool, bool) {
        let (dx, dy) = self.axis();
        if dx.abs() >= dy.abs() {
            (true, dx < 0.0)
        } else {
            (false, dy < 0.0)
        }
    }
}

/// Most decoded glyphs fit in eight UTF-8 bytes, including expanded ligatures.
#[derive(PartialEq, Eq)]
pub(crate) enum GlyphText {
    Inline { bytes: [u8; 8], len: u8 },
    Long(Box<str>),
}

impl From<&str> for GlyphText {
    fn from(text: &str) -> Self {
        let mut bytes = [0; 8];
        if let Some(destination) = bytes.get_mut(..text.len()) {
            destination.copy_from_slice(text.as_bytes());
            Self::Inline {
                bytes,
                len: text.len() as u8,
            }
        } else {
            Self::Long(text.into())
        }
    }
}

impl GlyphText {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Inline { bytes, len } => {
                std::str::from_utf8(&bytes[..usize::from(*len)]).expect("copied UTF-8")
            }
            Self::Long(text) => text,
        }
    }
}

pub(crate) struct Glyph {
    pub text: GlyphText,
    pub bbox: BoundingBox,
    pub font: usize,
    pub size: f64,
    pub color: Color,
    pub direction: Direction,
    pub rotated: bool,
    pub start: f64,
    pub end: f64,
    pub baseline: f64,
    pub rise: f64,
    pub space_width: f64,
    pub char_space: f64,
}

struct Interpreter<'a> {
    doc: &'a Document,
    fonts: Vec<Font>,
    font_ids: HashMap<*const Dictionary, usize>,
    glyphs: Vec<Glyph>,
    graphics: Vec<GraphicObject>,
    active_forms: Vec<*const Stream>,
    remaining: usize,
}

impl<'a> Interpreter<'a> {
    fn resource(
        &self,
        resources: Option<&'a Dictionary>,
        kind: &[u8],
        name: &[u8],
    ) -> Option<&'a Object> {
        let entries = entry(self.doc, resources?, kind)?.as_dict().ok()?;
        entry(self.doc, entries, name)
    }

    fn font(&mut self, resources: Option<&'a Dictionary>, name: &[u8]) -> Option<usize> {
        let font = self.resource(resources, b"Font", name)?.as_dict().ok()?;
        let key = std::ptr::from_ref(font);
        Some(*self.font_ids.entry(key).or_insert_with(|| {
            let id = self.fonts.len();
            self.fonts.push(Font::new(self.doc, font, name));
            id
        }))
    }

    fn run(
        &mut self,
        data: &[u8],
        resources: Option<&'a Dictionary>,
        mut state: State,
    ) -> Result<(), String> {
        self.remaining = self
            .remaining
            .checked_sub(data.len())
            .ok_or("page and form content exceeds decode limit")?;
        let mut stack = Vec::new();
        let mut position = Position::default();
        let mut path = Path::default();
        let mut clipping = false;
        for operation in Content::decode_iter(data) {
            let operation = operation.map_err(|error| error.to_string())?;
            let args = operation.operands.as_slice();
            let name = || args.first()?.as_name().ok();
            let scalar = || args.first().and_then(number);
            let operator = operation.operator.as_str();
            match operator {
                "q" => stack.push(state),
                "Q" => {
                    if let Some(saved) = stack.pop() {
                        state = saved;
                    }
                }
                "cm" => {
                    if let Some(values) = numbers(args) {
                        state.ctm = state.ctm.then(Matrix(values));
                    }
                }
                "w" => state.line_width = scalar().unwrap_or(state.line_width).abs(),
                "g" | "G" | "rg" | "RG" | "k" | "K" | "sc" | "SC" | "scn" | "SCN" => {
                    let (color, components) = if operator.as_bytes()[0].is_ascii_uppercase() {
                        (&mut state.stroke, &mut state.stroke_components)
                    } else {
                        (&mut state.fill, &mut state.fill_components)
                    };
                    match operator {
                        "g" | "G" => *components = 1,
                        "rg" | "RG" => *components = 3,
                        "k" | "K" => *components = 4,
                        _ => {}
                    }
                    *color = read_color(args, *components).unwrap_or(*color);
                }
                "cs" | "CS" => {
                    if let Some(name) = name() {
                        let components = self.color_components(resources, name);
                        let initial = (0, 0, 0);
                        if operator == "cs" {
                            state.fill_components = components;
                            state.fill = initial;
                        } else {
                            state.stroke_components = components;
                            state.stroke = initial;
                        }
                    }
                }
                "BT" => {
                    position = Position {
                        active: true,
                        ..Position::default()
                    }
                }
                "ET" => position.active = false,
                "Tf" => {
                    if let (Some(name), Some(size)) = (name(), args.get(1).and_then(number)) {
                        state.text.font = self.font(resources, name);
                        state.text.size = size;
                    }
                }
                "Tc" => state.text.char_space = scalar().unwrap_or(state.text.char_space),
                "Tw" => state.text.word_space = scalar().unwrap_or(state.text.word_space),
                "Tz" => {
                    state.text.hscale = scalar().map_or(state.text.hscale, |value| value / 100.0)
                }
                "TL" => state.text.leading = scalar().unwrap_or(state.text.leading),
                "Tr" => {
                    state.text.render = args
                        .first()
                        .and_then(|obj| obj.as_i64().ok())
                        .unwrap_or(state.text.render)
                }
                "Ts" => state.text.rise = scalar().unwrap_or(state.text.rise),
                "Td" | "TD" => {
                    if let Some([x, y]) = numbers(args) {
                        if operator == "TD" {
                            state.text.leading = -y;
                        }
                        position.move_line(x, y);
                    }
                }
                "Tm" => {
                    if let Some(values) = numbers(args) {
                        position.text = Matrix(values);
                        position.line = position.text;
                    }
                }
                "T*" => position.move_line(0.0, -state.text.leading),
                "Tj" | "'" | "\"" => {
                    if operator == "\""
                        && let Some([word, character]) = numbers(args)
                    {
                        state.text.word_space = word;
                        state.text.char_space = character;
                    }
                    if operator != "Tj" {
                        position.move_line(0.0, -state.text.leading);
                    }
                    if let Some(bytes) = args
                        .get(if operator == "\"" { 2 } else { 0 })
                        .and_then(|obj| obj.as_str().ok())
                    {
                        self.show(bytes, &mut position, state);
                    }
                }
                "TJ" => {
                    if let Some(array) = args.first().and_then(|obj| obj.as_array().ok()) {
                        for item in array {
                            if let Ok(bytes) = item.as_str() {
                                self.show(bytes, &mut position, state);
                            } else if let Some(kern) = number(item) {
                                let delta = -kern * state.text.size / 1000.0;
                                if state.text.font.is_some_and(|id| self.fonts[id].vertical) {
                                    position.advance(0.0, delta);
                                } else {
                                    position.advance(delta * state.text.hscale, 0.0);
                                }
                            }
                        }
                    }
                }
                "Do" => {
                    if let Some(name) = name() {
                        self.xobject(name, resources, state)?;
                    }
                }
                "BI" => self.graphic(
                    state.ctm.rect(state.clip.page, 0.0, 0.0, 1.0, 1.0),
                    GraphicKind::Image,
                    state.clip,
                ),
                "m" | "l" => {
                    if let Some([x, y]) = numbers(args) {
                        let point = state.ctm.point(x, y);
                        if operator == "m" {
                            path.start(point);
                        } else {
                            path.push(point);
                        }
                    }
                }
                "c" | "v" | "y" => path.curve(),
                "re" => {
                    if let Some([x, y, w, h]) = numbers(args) {
                        let points = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
                            .map(|(x, y)| state.ctm.point(x, y));
                        path.curved = false;
                        path.subpaths.push(Subpath {
                            points: points.into(),
                            closed: true,
                            rectangle: true,
                        });
                    }
                }
                "h" => path.close(),
                "W" | "W*" => clipping = true,
                "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                    if matches!(operator, "s" | "b" | "b*") {
                        path.close();
                    }
                    if operator != "n" {
                        self.paint_path(
                            &path,
                            state,
                            matches!(operator, "S" | "s" | "B" | "B*" | "b" | "b*"),
                            !matches!(operator, "S" | "s"),
                        );
                    }
                    if clipping && let Some(rect) = path.rectangular_clip(state.clip.page) {
                        state.clip = state.clip.intersection(&rect, false).unwrap_or_else(|| {
                            BoundingBox::new(state.clip.page, 0.0, 0.0, 0.0, 0.0)
                        });
                    }
                    path.clear();
                    clipping = false;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn color_components(&self, resources: Option<&'a Dictionary>, name: &[u8]) -> usize {
        let resource = self.resource(resources, b"ColorSpace", name);
        let array = resource.and_then(|obj| obj.as_array().ok());
        let named = resource
            .and_then(|obj| obj.as_name().ok())
            .or_else(|| array?.first()?.as_name().ok())
            .unwrap_or(name);
        match named {
            b"DeviceRGB" | b"RGB" | b"CalRGB" | b"Lab" => 3,
            b"DeviceCMYK" | b"CMYK" => 4,
            b"ICCBased" => array
                .and_then(|array| resolve(self.doc, array.get(1)?)?.as_stream().ok())
                .and_then(|stream| entry(self.doc, &stream.dict, b"N")?.as_i64().ok())
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(3),
            b"Pattern" => 0,
            _ => 1,
        }
    }

    fn show(&mut self, mut bytes: &[u8], position: &mut Position, state: State) {
        let Some(font_id) = state.text.font.filter(|_| position.active) else {
            return;
        };
        let font = &self.fonts[font_id];
        let t = state.text;
        let [a, b, c, d, _, _] = state.ctm.then(position.text).0;
        let (dx, dy) = if font.vertical {
            (-c, -d)
        } else {
            (a * t.hscale, b * t.hscale)
        };
        let direction = Direction::from_vector(dx * t.size.signum(), dy * t.size.signum());
        let (cosine, sine) = direction.axis();
        let project = |(x, y): Point| (x * cosine + y * sine, y * cosine - x * sine);
        let size = c.hypot(d) * t.size.abs();
        let horizontal_scale = a.hypot(b) * t.hscale.abs();
        let space_width = font.space_width() * t.size.abs() * horizontal_scale / 1000.0;
        let character_spacing = if font.vertical {
            t.char_space * (-size / t.size)
        } else {
            t.char_space * horizontal_scale * t.size.signum()
        };
        while !bytes.is_empty() {
            let glyph = font.decode(bytes);
            bytes = &bytes[glyph.byte_len..];
            let spacing = t.char_space + if glyph.word_space { t.word_space } else { 0.0 };
            let matrix = state.ctm.then(position.text);
            let text_matrix = matrix
                .then(Matrix::translation(0.0, t.rise))
                .then(Matrix::scale(t.size * t.hscale / 1000.0, t.size / 1000.0));
            let origin = text_matrix.point(0.0, 0.0);
            let edge = if font.vertical {
                text_matrix.point(0.0, glyph.vertical_metric[0])
            } else {
                text_matrix.point(glyph.width, 0.0)
            };
            let (start, baseline) = project(origin);
            let (end, _) = project(edge);
            let (_, base_without_rise) = project(matrix.point(0.0, 0.0));
            let bbox = if font.vertical {
                let [_, vx, vy] = glyph.vertical_metric;
                text_matrix.rect(
                    state.clip.page,
                    -vx,
                    font.descent * 1000.0 - vy,
                    glyph.width - vx,
                    font.ascent * 1000.0 - vy,
                )
            } else {
                text_matrix.rect(
                    state.clip.page,
                    0.0,
                    font.descent * 1000.0,
                    glyph.width,
                    font.ascent * 1000.0,
                )
            };
            // Mode 3 commonly carries the searchable OCR layer on scanned pages.
            if size.is_finite()
                && size > 0.01
                && t.render != 7
                && !glyph.text.is_empty()
                && start.is_finite()
                && end.is_finite()
                && baseline.is_finite()
                && let Some(bbox) = state.clip.intersection(&bbox, true)
            {
                self.glyphs.push(Glyph {
                    text: glyph.text.as_ref().into(),
                    bbox,
                    font: font_id,
                    size,
                    color: if matches!(t.render, 1 | 5) {
                        state.stroke
                    } else {
                        state.fill
                    },
                    direction,
                    rotated: direction != Direction::Right,
                    start,
                    end,
                    baseline,
                    rise: base_without_rise - baseline,
                    space_width,
                    char_space: character_spacing,
                });
            }
            if font.vertical {
                position.advance(0.0, glyph.vertical_metric[0] * t.size / 1000.0 + spacing);
            } else {
                position.advance((glyph.width * t.size / 1000.0 + spacing) * t.hscale, 0.0);
            }
        }
    }

    fn xobject(
        &mut self,
        name: &[u8],
        resources: Option<&'a Dictionary>,
        mut state: State,
    ) -> Result<(), String> {
        let Some(stream) = self
            .resource(resources, b"XObject", name)
            .and_then(|obj| obj.as_stream().ok())
        else {
            return Ok(());
        };
        match entry(self.doc, &stream.dict, b"Subtype").and_then(|obj| obj.as_name().ok()) {
            Some(b"Image") => self.graphic(
                state.ctm.rect(state.clip.page, 0.0, 0.0, 1.0, 1.0),
                GraphicKind::Image,
                state.clip,
            ),
            Some(b"Form") => {
                let id = std::ptr::from_ref(stream);
                if self.active_forms.len() >= FORM_DEPTH_LIMIT || self.active_forms.contains(&id) {
                    return Ok(());
                }
                let form = |key: &[u8]| entry(self.doc, &stream.dict, key);
                if let Some(values) = form(b"Matrix").and_then(|obj| numbers(obj.as_array().ok()?))
                {
                    state.ctm = state.ctm.then(Matrix(values));
                }
                if let Some([x0, y0, x1, y1]) =
                    form(b"BBox").and_then(|obj| numbers(obj.as_array().ok()?))
                {
                    let bbox = state.ctm.rect(state.clip.page, x0, y0, x1, y1);
                    let Some(clip) = state.clip.intersection(&bbox, false) else {
                        return Ok(());
                    };
                    state.clip = clip;
                }
                let local = form(b"Resources")
                    .and_then(|obj| obj.as_dict().ok())
                    .or(resources);
                let bytes = stream_bytes(self.doc, stream).map_err(|error| error.to_string())?;
                self.active_forms.push(id);
                let result = self.run(&bytes, local, state);
                self.active_forms.pop();
                result?;
            }
            _ => {}
        }
        Ok(())
    }

    fn graphic(&mut self, bbox: BoundingBox, kind: GraphicKind, clip: BoundingBox) {
        if let Some(bbox) = clip.intersection(&bbox, false) {
            self.graphics.push(GraphicObject {
                page: bbox.page,
                bbox,
                kind,
            });
        }
    }

    fn paint_path(&mut self, path: &Path, state: State, stroke: bool, fill: bool) {
        let [a, b, c, d, _, _] = state.ctm.0;
        let thickness = (state.line_width * a.hypot(b).max(c.hypot(d))).max(0.1);
        for subpath in &path.subpaths {
            if (subpath.rectangle || subpath.is_axis_aligned_rectangle())
                && (subpath.closed || fill || subpath.ends_at_start())
                && let Some(mut bbox) = bounds(state.clip.page, &subpath.points)
                && let Some(kind) = rectangle_kind(&bbox)
            {
                if stroke && bbox.height == 0.0 {
                    bbox.y -= thickness / 2.0;
                    bbox.height = thickness;
                } else if stroke && bbox.width == 0.0 {
                    bbox.x -= thickness / 2.0;
                    bbox.width = thickness;
                }
                self.graphic(bbox, kind, state.clip);
            } else if stroke {
                let segments = subpath.points.windows(2).map(|pair| [pair[0], pair[1]]);
                let closing = subpath
                    .closed
                    .then(|| Some([*subpath.points.last()?, *subpath.points.first()?]))
                    .flatten();
                for points in segments.chain(closing) {
                    let Some(mut bbox) = bounds(state.clip.page, &points) else {
                        continue;
                    };
                    if bbox.width >= 3.0 && bbox.height < 0.65 {
                        bbox.y -= thickness / 2.0;
                        bbox.height += thickness;
                        self.graphic(bbox, GraphicKind::HorizontalRule, state.clip);
                    } else if bbox.height >= 3.0 && bbox.width < 0.65 {
                        bbox.x -= thickness / 2.0;
                        bbox.width += thickness;
                        self.graphic(bbox, GraphicKind::VerticalRule, state.clip);
                    }
                }
            }
        }
    }
}

fn read_color(args: &[Object], components: usize) -> Option<Color> {
    let byte = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    match components {
        1 => {
            let gray = byte(number(args.first()?)?);
            Some((gray, gray, gray))
        }
        3 => {
            let [r, g, b] = numbers(args)?;
            Some((byte(r), byte(g), byte(b)))
        }
        4 => {
            let [c, m, y, k] = numbers(args)?;
            Some((byte(1.0 - c - k), byte(1.0 - m - k), byte(1.0 - y - k)))
        }
        _ => None,
    }
}

fn rectangle_kind(bbox: &BoundingBox) -> Option<GraphicKind> {
    if bbox.width >= 3.0 && bbox.height <= 3.0 && bbox.width > 4.0 * bbox.height {
        Some(GraphicKind::HorizontalRule)
    } else if bbox.height >= 3.0 && bbox.width <= 3.0 && bbox.height > 4.0 * bbox.width {
        Some(GraphicKind::VerticalRule)
    } else if bbox.width >= 3.0 && bbox.height >= 3.0 {
        Some(GraphicKind::VectorBox)
    } else {
        None
    }
}

#[derive(Default)]
struct Subpath {
    points: Vec<Point>,
    closed: bool,
    rectangle: bool,
}

impl Subpath {
    fn ends_at_start(&self) -> bool {
        let Some((&(x, y), &(last_x, last_y))) = self.points.first().zip(self.points.last()) else {
            return false;
        };
        (x - last_x).abs() < 0.1 && (y - last_y).abs() < 0.1
    }

    fn is_axis_aligned_rectangle(&self) -> bool {
        let (a, b, c, d) = match self.points.as_slice() {
            [a, b, c, d] => (*a, *b, *c, *d),
            [a, b, c, d, _] if self.ends_at_start() => (*a, *b, *c, *d),
            _ => return false,
        };
        let near = |a: f64, b: f64| (a - b).abs() < 0.1;
        let axis_aligned = |a: Point, b: Point, c: Point, d: Point| {
            near(a.0, b.0) && near(b.1, c.1) && near(c.0, d.0) && near(d.1, a.1)
        };
        axis_aligned(a, b, c, d) || axis_aligned(b, c, d, a)
    }
}

#[derive(Default)]
struct Path {
    subpaths: Vec<Subpath>,
    curved: bool,
}

impl Path {
    fn start(&mut self, point: Point) {
        self.subpaths.push(Subpath {
            points: vec![point],
            ..Subpath::default()
        });
        self.curved = false;
    }

    fn push(&mut self, point: Point) {
        if !self.curved
            && let Some(path) = self.subpaths.last_mut()
        {
            path.rectangle = false;
            path.points.push(point);
        }
    }

    fn curve(&mut self) {
        if !self.curved
            && let Some(path) = self.subpaths.last_mut()
        {
            // Curved artwork has no rule or rectangle geometry to retain.
            path.points.clear();
            path.rectangle = false;
        }
        self.curved = true;
    }

    fn close(&mut self) {
        if !self.curved
            && let Some(path) = self.subpaths.last_mut()
        {
            path.closed = true;
        }
    }

    fn clear(&mut self) {
        self.subpaths.clear();
        self.curved = false;
    }

    fn rectangular_clip(&self, page: usize) -> Option<BoundingBox> {
        let [path] = self.subpaths.as_slice() else {
            return None;
        };
        path.is_axis_aligned_rectangle()
            .then(|| bounds(page, &path.points))
            .flatten()
    }
}

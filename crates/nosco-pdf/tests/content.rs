use std::io::Write;

use nosco_pdf::{BoundingBox, GraphicKind, PdfError, extract_from_bytes};
use lopdf::{
    Dictionary, Document, EncryptionState, EncryptionVersion, Object, Permissions, Stream,
    dictionary,
};

fn simple_font(base_font: &str) -> Dictionary {
    dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => base_font, "Encoding" => "WinAnsiEncoding" }
}

fn pdf(
    contents: impl Into<Object>,
    resources: Dictionary,
    page_tree: Dictionary,
    mut doc: Document,
) -> Document {
    let pages = doc.new_object_id();
    let page = doc.add_object(
        dictionary! { "Type" => "Page", "Parent" => pages, "Contents" => contents.into() },
    );
    let mut tree = dictionary! {
        "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 160.into()],
        "CropBox" => vec![10.into(), 20.into(), 190.into(), 140.into()],
        "Resources" => resources,
    };
    tree.extend(&page_tree);
    doc.objects.insert(pages, tree.into());
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    let info = doc.add_object(dictionary! { "Title" => lopdf::text_string("Étude"), "Author" => lopdf::text_string("Łukasz") });
    doc.trailer.set("Root", catalog);
    doc.trailer.set("Info", info);
    doc
}

fn bytes(mut doc: Document) -> Vec<u8> {
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
}

#[test]
fn transforms_text_kerning_colors_scripts_and_metadata() {
    let mut source = Document::with_version("1.5");
    // A text object intentionally crosses the boundary between content streams.
    let mut compressed = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    compressed
        .write_all(b"q 1 0 0 rg BT /F1 12 Tf 1 0 0 1 30 117 Tm [(Hello) -240 (world!)] TJ")
        .unwrap();
    let binary = compressed.finish().unwrap();
    let mut ascii_hex = binary
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>()
        .into_bytes();
    ascii_hex.push(b'>');
    let filters = source.add_object(Object::Array(vec![
        Object::Name(b"AHx".to_vec()),
        Object::Name(b"Fl".to_vec()),
    ]));
    let first = source.add_object(Stream::new(dictionary! { "Filter" => filters }, ascii_hex));
    let rest = source.add_object(Stream::new(
        Dictionary::new(),
        br#"
        ET Q
        BT /F1 12 Tf 1 0 0 1 30 96 Tm (x) Tj /F1 7 Tf 4 Ts (2) Tj -3 Ts (i) Tj 0 Ts ET
        BT /F1 12 Tf 2 Tc 0 0.5 1 RG 1 Tr 1 0 0 1 30 75 Tm (tracked) Tj
           0 Tr 14 TL (Next) ' 4 0 (A B) " T* (last) Tj ET
    "#
        .to_vec(),
    ));
    let doc = pdf(
        vec![first.into(), rest.into()],
        dictionary! { "Font" => dictionary! { "F1" => simple_font("Helvetica") } },
        Dictionary::new(),
        source,
    );
    let doc = extract_from_bytes(&bytes(doc)).unwrap();
    assert_eq!(
        doc.full_text(),
        "Hello world!\nx2i\ntracked\nNext\nA B\nlast"
    );
    assert_eq!(doc.metadata.title.as_deref(), Some("Étude"));
    assert_eq!(doc.metadata.author.as_deref(), Some("Łukasz"));
    assert_eq!(doc.dominant_font_size(), 12.0);
    let page = &doc.pages[0];
    assert_eq!((page.number, page.width, page.height), (1, 180.0, 120.0));
    let hello = &doc.tokens[0];
    assert_eq!(hello.color, (255, 0, 0));
    assert_eq!(hello.font_name, "Helvetica");
    assert!(hello.space_after);
    near(hello.bbox.x, 20.0);
    near(hello.bbox.y, 140.0 - (117.0 + 12.0 * 0.718));
    assert_eq!(doc.tokens[2].text, "!");
    assert!(doc.tokens[2].new_line_after);
    assert!(
        doc.tokens
            .iter()
            .any(|token| token.text == "2" && token.superscript && !token.subscript)
    );
    assert!(
        doc.tokens
            .iter()
            .any(|token| token.text == "i" && token.subscript && !token.superscript)
    );
    let tracked = doc
        .tokens
        .iter()
        .find(|token| token.text == "tracked")
        .unwrap();
    assert_eq!(tracked.color, (0, 128, 255));
    assert!(doc.block_tokens(100).is_empty());
    assert_eq!(doc.line_tokens(100).len(), 0);
}

#[test]
fn rotated_form_local_fonts_images_and_vector_rules() {
    let mut source = Document::with_version("1.5");
    let image = source.add_object(Stream::new(dictionary! {
        "Subtype" => "Image", "Width" => 1, "Height" => 1, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
    }, vec![255]));
    let form = source.add_object(Stream::new(
        dictionary! {
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 40.into(), 40.into()],
            "Matrix" => vec![2.into(), 0.into(), 0.into(), 2.into(), 0.into(), 0.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => simple_font("Times-BoldItalic") },
                "XObject" => dictionary! { "Im" => image },
            },
        },
        br#"
        q 10 0 0 8 2 4 cm /Im Do Q
        BT /F1 6 Tf 2 24 Td (Form words) Tj ET
        0.5 w 3 15 m 13 15 l S
        14 15 8 0.4 re f
        14 18 8 4 re B
        0 0 m 6 6 l n
    "#
        .to_vec(),
    ));
    let stream = source.add_object(Stream::new(
        Dictionary::new(),
        br#"
        q 1 0 0 1 30 40 cm /Fx Do Q
        q 2 0 0 3 70 80 cm BI /W 1 /H 1 /CS /G /BPC 8 ID X EI Q
    "#
        .to_vec(),
    ));
    let doc = pdf(
        stream,
        dictionary! {
            "Font" => dictionary! { "F1" => simple_font("Helvetica") },
            "XObject" => dictionary! { "Fx" => form },
        },
        dictionary! { "Rotate" => 90 },
        source,
    );
    let doc = extract_from_bytes(&bytes(doc)).unwrap();
    assert_eq!(doc.full_text(), "Form words");
    let page = &doc.pages[0];
    assert_eq!((page.width, page.height), (120.0, 180.0));
    assert!(doc.tokens.iter().all(|token| token.bold
        && token.italic
        && token.rotated
        && token.font_name == "Times-BoldItalic"));
    assert_eq!(
        doc.graphics
            .iter()
            .filter(|graphic| graphic.kind == GraphicKind::Image)
            .count(),
        2
    );
    let image = doc
        .graphics
        .iter()
        .find(|graphic| graphic.kind == GraphicKind::Image)
        .unwrap();
    assert_eq!(image.bbox, BoundingBox::new(1, 28.0, 24.0, 16.0, 20.0));
    assert_eq!(
        doc.graphics
            .iter()
            .filter(|graphic| graphic.kind == GraphicKind::VerticalRule)
            .count(),
        2
    );
    assert_eq!(
        doc.graphics
            .iter()
            .filter(|graphic| graphic.kind == GraphicKind::VectorBox)
            .count(),
        1
    );
}

#[test]
fn encrypted_pdf_with_empty_and_nonempty_passwords() {
    for password in ["", "required"] {
        let mut source = Document::with_version("1.5");
        let stream = source.add_object(Stream::new(
            Dictionary::new(),
            b"BT /F1 12 Tf 20 100 Td (Encrypted text) Tj ET".to_vec(),
        ));
        let mut doc = pdf(
            stream,
            dictionary! { "Font" => dictionary! { "F1" => simple_font("Helvetica") } },
            Dictionary::new(),
            source,
        );
        doc.trailer.set(
            "ID",
            Object::Array(vec![
                Object::string_literal("test file ID a"),
                Object::string_literal("test file ID b"),
            ]),
        );
        let state = EncryptionState::try_from(EncryptionVersion::V2 {
            document: &doc,
            owner_password: "owner",
            user_password: password,
            key_length: 128,
            permissions: Permissions::all(),
        })
        .unwrap();
        doc.encrypt(&state).unwrap();
        let result = extract_from_bytes(&bytes(doc));
        if password.is_empty() {
            assert_eq!(result.unwrap().full_text(), "Encrypted text");
        } else {
            assert!(matches!(result, Err(PdfError::Encrypted(_))));
        }
    }
}

#[test]
fn ascii85_and_lzw_content_stream() {
    let mut source = Document::with_version("1.5");
    let content = b"BT /F1 9 Tf 40 90 Td (Packed text) Tj ET";
    // Literal LZW codes below the point at which 9-bit codes widen.
    let mut raw = Vec::new();
    let mut buffered = 0u32;
    let mut bits = 0;
    for code in [256]
        .into_iter()
        .chain(content.iter().map(|&byte| u32::from(byte)))
        .chain([257])
    {
        buffered = (buffered << 9) | code;
        bits += 9;
        while bits >= 8 {
            bits -= 8;
            raw.push((buffered >> bits) as u8);
        }
    }
    if bits != 0 {
        raw.push((buffered << (8 - bits)) as u8);
    }
    let mut armored = Vec::new();
    for chunk in raw.chunks(4) {
        let mut padded = [0u8; 4];
        padded[..chunk.len()].copy_from_slice(chunk);
        let mut value = u32::from_be_bytes(padded);
        let mut digits = [0u8; 5];
        for digit in digits.iter_mut().rev() {
            *digit = b'!' + (value % 85) as u8;
            value /= 85;
        }
        armored.extend_from_slice(&digits[..chunk.len() + 1]);
    }
    armored.extend_from_slice(b"~>");
    let stream = source.add_object(Stream::new(dictionary! {
        "Filter" => vec![Object::Name(b"ASCII85Decode".to_vec()), Object::Name(b"LZWDecode".to_vec())],
    }, armored));
    let doc = pdf(
        stream,
        dictionary! { "Font" => dictionary! { "F1" => simple_font("Helvetica") } },
        Dictionary::new(),
        source,
    );
    assert_eq!(
        extract_from_bytes(&bytes(doc)).unwrap().full_text(),
        "Packed text"
    );
}

#[test]
fn cid_font_to_unicode_and_proportional_token_boxes() {
    let mut source = Document::with_version("1.5");
    let unicode = source.add_object(Stream::new(
        Dictionary::new(),
        br#"
        1 begincodespacerange <0000> <ffff> endcodespacerange
        2 beginbfchar <0001> <00660069002C> <0002> <00A0> endbfchar
        1 beginbfrange <0003> <0004> [<03B1> <D83DDE00>] endbfrange
    "#
        .to_vec(),
    ));
    let cid_font = source.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => "TestFont",
        "DW" => 500,
        "W" => vec![1.into(), vec![900.into(), 300.into()].into(), 3.into(), 4.into(), 700.into()],
    });
    let font = source.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "TestFont",
        "Encoding" => "Identity-H", "DescendantFonts" => vec![cid_font.into()], "ToUnicode" => unicode,
    });
    let stream = source.add_object(Stream::new(
        Dictionary::new(),
        b"BT /F1 10 Tf 20 90 Td <0001000200030004> Tj ET".to_vec(),
    ));
    let doc = pdf(
        stream,
        dictionary! { "Font" => dictionary! { "F1" => font } },
        Dictionary::new(),
        source,
    );
    let doc = extract_from_bytes(&bytes(doc)).unwrap();
    assert_eq!(doc.full_text(), "fi, α😀");
    assert_eq!(doc.tokens.len(), 3);
    near(doc.tokens[0].bbox.width, 6.0);
    near(doc.tokens[1].bbox.x, 16.0);
    near(doc.tokens[1].bbox.width, 3.0);
    near(doc.tokens[2].bbox.x, 22.0);
    near(doc.tokens[2].bbox.width, 14.0);
}

#[test]
fn windows_pages_recovers_bad_content_and_reads_catalog_metadata() {
    use nosco_pdf::{ExtractOptions, extract_from_bytes_with_options};

    let mut source = Document::with_version("1.5");
    let invalid = source.add_object(Stream::new(
        dictionary! { "Filter" => "JBIG2Decode" },
        b"broken".to_vec(),
    ));
    let mut source = pdf(invalid, Dictionary::new(), Dictionary::new(), source);
    let root = source.trailer.get(b"Root").unwrap().as_reference().unwrap();
    let tree = source
        .get_dictionary(root)
        .unwrap()
        .get(b"Pages")
        .unwrap()
        .as_reference()
        .unwrap();
    let second = source.add_object(dictionary! { "Type" => "Page", "Parent" => tree });
    let third = source.add_object(dictionary! { "Type" => "Page", "Parent" => tree });
    let pages = source.get_object_mut(tree).unwrap().as_dict_mut().unwrap();
    pages
        .get_mut(b"Kids")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .extend([second.into(), third.into()]);
    pages.set("Count", 3);
    let xmp = source.add_object(Stream::new(dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        br#"<rdf:Description><prism:doi>10.1007/example123</prism:doi><dc:title>Catalog title</dc:title></rdf:Description>"#.to_vec()));
    source
        .get_object_mut(root)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("Metadata", xmp);
    let info = source.trailer.get(b"Info").unwrap().as_reference().unwrap();
    source
        .get_object_mut(info)
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .set("WPS-ARTICLEDOI", lopdf::text_string("10.1007/example123"));

    let data = bytes(source);
    let layout =
        extract_from_bytes_with_options(&data, ExtractOptions { max_pages: Some(2) }).unwrap();
    assert_eq!(
        layout
            .pages
            .iter()
            .map(|page| page.number)
            .collect::<Vec<_>>(),
        [2]
    );
    assert_eq!(layout.metadata.xmp["dc:title"], ["Catalog title"]);
    assert_eq!(layout.metadata.doi.as_deref(), Some("10.1007/example123"));
    assert!(matches!(
        extract_from_bytes_with_options(&data, ExtractOptions { max_pages: Some(1) }),
        Err(PdfError::Page { page: 1, .. })
    ));
    assert_eq!(extract_from_bytes(&data).unwrap().pages.len(), 2);
}

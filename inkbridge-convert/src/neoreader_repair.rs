pub(crate) fn repair_trailing_xref_stream_length(bytes: &mut [u8]) -> Result<bool, String> {
    let startxref = rfind_bytes(bytes, b"startxref")
        .ok_or_else(|| "NeoReader repair could not find startxref".to_owned())?;
    let mut offset_cursor = startxref + b"startxref".len();
    let xref_offset = parse_pdf_integer(bytes, &mut offset_cursor)
        .ok_or_else(|| "NeoReader repair found an invalid startxref offset".to_owned())?;
    if xref_offset >= startxref {
        return Err("NeoReader repair found an out-of-range xref offset".to_owned());
    }
    let xref = &bytes[xref_offset..startxref];
    let stream_relative = find_bytes(xref, b"stream")
        .ok_or_else(|| "NeoReader repair found no trailing xref stream".to_owned())?;
    let dictionary = &xref[..stream_relative];
    if pdf_name_value(dictionary, b"Type").as_deref() != Some(b"XRef") {
        return Ok(false);
    }
    let widths = pdf_integer_array(dictionary, b"W")
        .ok_or_else(|| "NeoReader xref stream has no valid /W array".to_owned())?;
    if widths.len() != 3 {
        return Err("NeoReader xref stream /W must contain three integers".to_owned());
    }
    let entry_width = widths
        .iter()
        .try_fold(0usize, |total, width| total.checked_add(*width))
        .ok_or_else(|| "NeoReader xref entry width overflowed".to_owned())?;
    let entry_count = if let Some(index) = pdf_integer_array(dictionary, b"Index") {
        if index.len() % 2 != 0 {
            return Err("NeoReader xref stream /Index has an odd number of integers".to_owned());
        }
        index
            .chunks_exact(2)
            .try_fold(0usize, |total, pair| total.checked_add(pair[1]))
            .ok_or_else(|| "NeoReader xref entry count overflowed".to_owned())?
    } else {
        pdf_integer_value(dictionary, b"Size")
            .ok_or_else(|| "NeoReader xref stream has neither /Index nor /Size".to_owned())?
    };
    let expected_length = entry_width
        .checked_mul(entry_count)
        .ok_or_else(|| "NeoReader xref stream length overflowed".to_owned())?;

    let stream_keyword_end = xref_offset + stream_relative + b"stream".len();
    let data_start = match bytes.get(stream_keyword_end..) {
        Some([b'\r', b'\n', ..]) => stream_keyword_end + 2,
        Some([b'\n', ..]) | Some([b'\r', ..]) => stream_keyword_end + 1,
        _ => return Err("NeoReader xref stream has no line ending after stream".to_owned()),
    };
    let data_end = data_start
        .checked_add(expected_length)
        .ok_or_else(|| "NeoReader xref data range overflowed".to_owned())?;
    let endstream_start = match bytes.get(data_end..) {
        Some(tail) if tail.starts_with(b"endstream") => data_end,
        Some(tail) if tail.starts_with(b"\r\nendstream") => data_end + 2,
        Some(tail)
            if tail.starts_with(b"\nendstream")
                && bytes.get(data_end.wrapping_sub(1)) != Some(&b'\r') =>
        {
            data_end + 1
        }
        Some(tail) if tail.starts_with(b"\rendstream") => data_end + 1,
        _ => return Err("NeoReader xref /W and /Index do not lead to endstream".to_owned()),
    };
    if endstream_start >= startxref {
        return Err("NeoReader xref stream extends beyond startxref".to_owned());
    }

    let (length_start, length_end, current_length) = pdf_integer_span(dictionary, b"Length")
        .ok_or_else(|| "NeoReader xref stream has no direct integer /Length".to_owned())?;
    if current_length == expected_length {
        return Ok(false);
    }
    let width = length_end - length_start;
    let replacement = expected_length.to_string();
    if replacement.len() > width {
        return Err("NeoReader xref /Length cannot be repaired in place".to_owned());
    }
    let absolute_start = xref_offset + length_start;
    let padding = width - replacement.len();
    bytes[absolute_start..absolute_start + padding].fill(b'0');
    bytes[absolute_start + padding..absolute_start + width].copy_from_slice(replacement.as_bytes());
    Ok(true)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

fn pdf_integer_value(dictionary: &[u8], name: &[u8]) -> Option<usize> {
    pdf_integer_span(dictionary, name).map(|(_, _, value)| value)
}

fn pdf_integer_span(dictionary: &[u8], name: &[u8]) -> Option<(usize, usize, usize)> {
    let mut cursor = pdf_name_offset(dictionary, name)?;
    let start = skip_pdf_whitespace(dictionary, cursor);
    cursor = start;
    let value = parse_pdf_integer(dictionary, &mut cursor)?;
    Some((start, cursor, value))
}

fn pdf_integer_array(dictionary: &[u8], name: &[u8]) -> Option<Vec<usize>> {
    let mut cursor = skip_pdf_whitespace(dictionary, pdf_name_offset(dictionary, name)?);
    if dictionary.get(cursor) != Some(&b'[') {
        return None;
    }
    cursor += 1;
    let mut values = Vec::new();
    loop {
        cursor = skip_pdf_whitespace(dictionary, cursor);
        if dictionary.get(cursor) == Some(&b']') {
            return Some(values);
        }
        values.push(parse_pdf_integer(dictionary, &mut cursor)?);
    }
}

fn pdf_name_value(dictionary: &[u8], name: &[u8]) -> Option<Vec<u8>> {
    let cursor = skip_pdf_whitespace(dictionary, pdf_name_offset(dictionary, name)?);
    if dictionary.get(cursor) != Some(&b'/') {
        return None;
    }
    let start = cursor + 1;
    let end = dictionary[start..]
        .iter()
        .position(|byte| is_pdf_delimiter(*byte))
        .map(|relative| start + relative)
        .unwrap_or(dictionary.len());
    Some(dictionary[start..end].to_vec())
}

fn pdf_name_offset(dictionary: &[u8], name: &[u8]) -> Option<usize> {
    let mut needle = Vec::with_capacity(name.len() + 1);
    needle.push(b'/');
    needle.extend_from_slice(name);
    dictionary
        .windows(needle.len())
        .enumerate()
        .find_map(|(index, window)| {
            if window != needle.as_slice() {
                return None;
            }
            let end = index + needle.len();
            if dictionary
                .get(end)
                .is_none_or(|byte| is_pdf_delimiter(*byte))
            {
                Some(end)
            } else {
                None
            }
        })
}

fn parse_pdf_integer(bytes: &[u8], cursor: &mut usize) -> Option<usize> {
    *cursor = skip_pdf_whitespace(bytes, *cursor);
    let start = *cursor;
    while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
        *cursor += 1;
    }
    if *cursor == start {
        return None;
    }
    std::str::from_utf8(&bytes[start..*cursor])
        .ok()?
        .parse()
        .ok()
}

fn skip_pdf_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes
        .get(cursor)
        .is_some_and(|byte| matches!(byte, 0 | 9 | 10 | 12 | 13 | 32))
    {
        cursor += 1;
    }
    cursor
}

fn is_pdf_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        0 | 9
            | 10
            | 12
            | 13
            | 32
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xref_stream_with_length(declared_length: usize, data: &[u8]) -> Vec<u8> {
        let mut bytes = b"%PDF-1.5\n% test prefix\n".to_vec();
        let xref_offset = bytes.len();
        bytes.extend_from_slice(
            format!(
                "9 0 obj <</Type/XRef/W[1 2 1]/Index[0 2]/Length {declared_length}>>stream\r\n"
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(
            format!("\r\nendstream\nendobj\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
        );
        bytes
    }

    #[test]
    fn repairs_neoreader_trailing_xref_length_from_w_and_index() {
        let mut bytes = xref_stream_with_length(999, &[0; 8]);

        assert!(repair_trailing_xref_stream_length(&mut bytes).unwrap());
        assert!(bytes
            .windows(b"/Length 008".len())
            .any(|window| window == b"/Length 008"));
        assert!(!repair_trailing_xref_stream_length(&mut bytes).unwrap());
    }

    #[test]
    fn refuses_xref_length_repair_when_index_does_not_reach_endstream() {
        let mut bytes = xref_stream_with_length(999, &[0; 7]);
        let error = repair_trailing_xref_stream_length(&mut bytes)
            .expect_err("mismatched xref structure must not be guessed");
        assert!(error.contains("do not lead to endstream"));
    }
}

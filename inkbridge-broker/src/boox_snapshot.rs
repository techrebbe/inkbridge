use inkbridge_convert::{geometry_fingerprint, StrokeSnapshot};

const PDF_WIDTH_SCALE: f64 = 1.168_064_48;
const PDF_VIEW_COORDINATE_EPSILON: f64 = 1.0e-6;

/// Returns whether `observed` is the lossy standard-/Ink projection emitted by the
/// broker for `canonical`.
///
/// PDF /Ink preserves stable identity, page, visible geometry, width, and grayscale,
/// but it does not preserve the native pen type, layer, or per-sample pressure. Those
/// unrepresentable fields must not make a legitimate BOOX edit look stale; the visible
/// fields still form the optimistic-concurrency precondition.
pub(crate) fn boox_pdf_view_matches(canonical: &StrokeSnapshot, observed: &StrokeSnapshot) -> bool {
    observed.origin == "pdf-ink"
        && canonical.source_uuid == observed.source_uuid
        && canonical.page_index == observed.page_index
        && observed.native_style.layer_num == 0
        && observed.native_style.pen_type == 10
        && observed.native_style.thickness
            == projected_pdf_thickness(canonical.native_style.thickness)
        && observed.native_style.pen_color == normalized_supernote_color(canonical)
        && canonical.samples.len() == observed.samples.len()
        && canonical
            .samples
            .iter()
            .zip(&observed.samples)
            .all(|(expected, actual)| {
                (expected[0] - actual[0]).abs() <= PDF_VIEW_COORDINATE_EPSILON
                    && (expected[1] - actual[1]).abs() <= PDF_VIEW_COORDINATE_EPSILON
            })
}

/// Restores native-only metadata when NeoReader changed only the geometry of a
/// broker-generated standard /Ink annotation. Explicit visible style changes remain
/// authoritative; otherwise origin, native pen/layer, and pressure survive the trip.
pub(crate) fn restore_native_metadata(
    canonical: &StrokeSnapshot,
    observed_before: &StrokeSnapshot,
    after: &mut StrokeSnapshot,
) {
    after.origin.clone_from(&canonical.origin);
    if after.native_style.layer_num == observed_before.native_style.layer_num {
        after.native_style.layer_num = canonical.native_style.layer_num;
    }
    if after.native_style.pen_type == observed_before.native_style.pen_type {
        after.native_style.pen_type = canonical.native_style.pen_type;
    }
    if after.native_style.pen_color == observed_before.native_style.pen_color {
        after.native_style.pen_color = canonical.native_style.pen_color;
    }
    if after.native_style.thickness == observed_before.native_style.thickness {
        after.native_style.thickness = canonical.native_style.thickness;
        preserve_pressure_profile(&canonical.samples, &mut after.samples);
    }
    after.geometry_fingerprint = geometry_fingerprint(&after.native_style, &after.samples);
}

fn projected_pdf_thickness(thickness: i64) -> i64 {
    let width = ((thickness as f64 / 400.0) * PDF_WIDTH_SCALE).clamp(0.25, 12.0);
    ((width / PDF_WIDTH_SCALE) * 400.0)
        .round()
        .clamp(50.0, 4000.0) as i64
}

fn normalized_supernote_color(snapshot: &StrokeSnapshot) -> i64 {
    if snapshot.native_style.pen_color == 0 {
        0
    } else {
        0x9d
    }
}

fn preserve_pressure_profile(before: &[[f64; 3]], after: &mut [[f64; 3]]) {
    if before.is_empty() || after.is_empty() {
        return;
    }
    let last_before = before.len().saturating_sub(1);
    let last_after = after.len().saturating_sub(1).max(1);
    for (index, sample) in after.iter_mut().enumerate() {
        let source_index = ((index * last_before) as f64 / last_after as f64).round() as usize;
        sample[2] = before[source_index.min(last_before)][2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkbridge_convert::NativeStyle;

    fn snapshot(origin: &str, pen_type: i64, pressures: [f64; 2]) -> StrokeSnapshot {
        let native_style = NativeStyle {
            layer_num: if origin == "pdf-ink" { 0 } else { 3 },
            thickness: 413,
            pen_color: 0x9d,
            pen_type,
        };
        let samples = vec![[0.2, 0.3, pressures[0]], [0.4, 0.5, pressures[1]]];
        StrokeSnapshot {
            source_uuid: "stroke-1".to_owned(),
            origin: origin.to_owned(),
            page_index: 0,
            geometry_fingerprint: geometry_fingerprint(&native_style, &samples),
            native_style,
            samples,
        }
    }

    #[test]
    fn matches_only_the_visible_pdf_projection() {
        let canonical = snapshot("boox-neoreader", 16, [900.0, 2200.0]);
        let projected = snapshot("pdf-ink", 10, [1612.0, 1612.0]);
        assert!(boox_pdf_view_matches(&canonical, &projected));

        let mut moved = projected.clone();
        moved.samples[0][0] += 0.01;
        assert!(!boox_pdf_view_matches(&canonical, &moved));

        let mut restyled = projected;
        restyled.native_style.thickness += 1;
        assert!(!boox_pdf_view_matches(&canonical, &restyled));
    }

    #[test]
    fn restores_native_metadata_without_overwriting_a_visible_style_change() {
        let canonical = snapshot("boox-neoreader", 16, [900.0, 2200.0]);
        let projected = snapshot("pdf-ink", 10, [1612.0, 1612.0]);
        let mut moved = projected.clone();
        for sample in &mut moved.samples {
            sample[0] += 0.1;
        }
        restore_native_metadata(&canonical, &projected, &mut moved);
        assert_eq!(moved.origin, "boox-neoreader");
        assert_eq!(moved.native_style.layer_num, 3);
        assert_eq!(moved.native_style.pen_type, 16);
        assert_eq!(moved.samples[0][2], 900.0);
        assert_eq!(moved.samples[1][2], 2200.0);

        let mut restyled = projected.clone();
        restyled.native_style.thickness += 100;
        restore_native_metadata(&canonical, &projected, &mut restyled);
        assert_eq!(restyled.native_style.thickness, 513);
        assert_eq!(restyled.samples[0][2], 1612.0);
    }
}

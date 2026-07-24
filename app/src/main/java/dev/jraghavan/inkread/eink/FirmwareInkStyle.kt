package dev.jraghavan.inkread.eink

import android.graphics.Color

/**
 * Process-local style hand-off for device-native wet ink.
 *
 * InkRead stores stroke colours in the portable core as packed `r<<24|g<<16|b<<8|a`. BOOX
 * TouchHelper expects Android ARGB, so keep the currently selected Pen colour here in ARGB form.
 * Supernote ignores this value because its current firmware pen path is monochrome.
 */
internal object FirmwareInkStyle {
    @Volatile private var penColorArgb: Int = Color.BLACK

    fun setPenColorPacked(packed: Int) {
        val r = (packed ushr 24) and 0xFF
        val g = (packed ushr 16) and 0xFF
        val b = (packed ushr 8) and 0xFF
        val a = packed and 0xFF

        // Note Air 4C device test: TouchHelper accepts our black/red/blue values directly, but the
        // portable dark green (#2E7D32) falls back to black in the native wet-ink layer. Onyx's own
        // PenDemo uses #00B036 for GREEN, so use that calibrated preview colour while keeping the
        // portable/core pen colour unchanged. The baked stroke therefore remains device-neutral.
        penColorArgb = if (r == 0x2E && g == 0x7D && b == 0x32) {
            Color.argb(a, 0x00, 0xB0, 0x36)
        } else {
            Color.argb(a, r, g, b)
        }
    }

    fun currentPenColorArgb(): Int = penColorArgb
}

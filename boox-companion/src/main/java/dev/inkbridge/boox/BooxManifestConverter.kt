package dev.inkbridge.boox

import java.io.File

internal interface BooxManifestConverter {
    fun buildBaseline(pdf: File, sourceFileName: String): ByteArray
    fun buildManifest(pdf: File, baseline: ByteArray): ByteArray
}

internal class NativeBooxManifestConverter : BooxManifestConverter {
    override fun buildBaseline(pdf: File, sourceFileName: String): ByteArray =
        nativeBuildBaseline(pdf.absolutePath, sourceFileName)

    override fun buildManifest(pdf: File, baseline: ByteArray): ByteArray =
        nativeBuildManifest(pdf.absolutePath, baseline, PDF_TO_SUPERNOTE_Y_OFFSET)

    private external fun nativeBuildBaseline(path: String, sourceFileName: String): ByteArray

    private external fun nativeBuildManifest(
        path: String,
        baseline: ByteArray,
        normalizedYOffset: Double,
    ): ByteArray

    companion object {
        private const val PDF_TO_SUPERNOTE_Y_OFFSET = -0.0008

        init {
            System.loadLibrary("inkbridge_boox")
        }
    }
}

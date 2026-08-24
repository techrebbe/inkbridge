package dev.inkbridge.boox

import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files

class CompactBooxFinalizerTest {
    private val documentId = "inkbridge-doc-v1-" + "a".repeat(64)

    @Test
    fun prepares_baseline_then_publishes_idempotent_compact_manifest() {
        val root = Files.createTempDirectory("inkbridge-compact-finalize").toFile()
        val (store, state, active) = installedState(root)
        val converter = FakeConverter()
        val finalizer = CompactBooxFinalizer(store, converter)

        val baseline = finalizer.prepareBaseline(state)
        assertArrayEquals(FakeConverter.BASELINE, baseline.readBytes())
        assertEquals("book.pdf", converter.baselineSourceFileName)
        active.writeBytes("broker PDF plus NeoReader ink".toByteArray())

        val result = finalizer.finalize(documentId) as CompactFinalizeResult.Finalized
        assertEquals(1, result.operationCount)
        assertEquals(1, result.state.localGeneration)
        assertEquals(sha256Hex(active), result.state.finalizedLocalSha256)
        assertTrue(result.manifest.length() < active.length() * 20)
        val descriptor = JSONObject(result.descriptor.readText())
        assertEquals("boox_operation_manifest", descriptor.getString("payloadKind"))
        assertEquals(sha256Hex(result.manifest), descriptor.getString("contentSha256"))
        assertEquals(1, descriptor.getLong("sourceRevision"))
        assertEquals(0, descriptor.getJSONObject("basedOn").getLong("boox"))
        assertEquals(3, descriptor.getJSONObject("basedOn").getLong("supernote"))

        val repeated = finalizer.finalize(documentId) as CompactFinalizeResult.AlreadyFinalized
        assertEquals(result.manifest, repeated.manifest)
        assertEquals(1, converter.manifestCalls)
    }

    @Test
    fun changed_pdf_without_baseline_falls_back_without_publishing() {
        val root = Files.createTempDirectory("inkbridge-compact-fallback").toFile()
        val (store, _, active) = installedState(root)
        active.writeBytes("edited before baseline".toByteArray())

        val result = CompactBooxFinalizer(store, FakeConverter()).finalize(documentId)

        assertTrue(result is CompactFinalizeResult.FullPdfFallbackRequired)
        assertTrue(store.outgoingDirectory(documentId).listFiles().orEmpty().isEmpty())
        assertEquals(null, store.state(documentId)?.finalizedLocalSha256)
    }

    @Test
    fun invalid_native_manifest_falls_back_without_publishing() {
        val root = Files.createTempDirectory("inkbridge-compact-invalid").toFile()
        val (store, state, active) = installedState(root)
        val converter = object : BooxManifestConverter {
            override fun buildBaseline(pdf: File, sourceFileName: String) = FakeConverter.BASELINE
            override fun buildManifest(pdf: File, baseline: ByteArray) = "not-json".toByteArray()
        }
        val finalizer = CompactBooxFinalizer(store, converter)
        finalizer.prepareBaseline(state)
        active.writeBytes("edited PDF".toByteArray())

        val result = finalizer.finalize(documentId)

        assertTrue(result is CompactFinalizeResult.FullPdfFallbackRequired)
        assertTrue(store.outgoingDirectory(documentId).listFiles().orEmpty().isEmpty())
        assertEquals(null, store.state(documentId)?.finalizedLocalSha256)
    }

    @Test
    fun pdf_change_during_conversion_preserves_evidence_without_committing_state() {
        val root = Files.createTempDirectory("inkbridge-compact-race").toFile()
        val (store, state, active) = installedState(root)
        val converter = object : BooxManifestConverter {
            override fun buildBaseline(pdf: File, sourceFileName: String) = FakeConverter.BASELINE

            override fun buildManifest(pdf: File, baseline: ByteArray): ByteArray {
                val hashBeforeConcurrentEdit = sha256Hex(pdf)
                pdf.writeBytes("NeoReader continued changing the PDF".toByteArray())
                return FakeConverter.manifest(hashBeforeConcurrentEdit)
            }
        }
        val finalizer = CompactBooxFinalizer(store, converter)
        finalizer.prepareBaseline(state)
        active.writeBytes("first NeoReader edit".toByteArray())

        assertThrows(IllegalArgumentException::class.java) {
            finalizer.finalize(documentId)
        }

        assertEquals(null, store.state(documentId)?.finalizedLocalSha256)
        assertEquals(2, store.outgoingDirectory(documentId).listFiles().orEmpty().size)
    }

    private fun installedState(root: File): Triple<BooxHandoffStore, HandoffState, File> {
        val store = BooxHandoffStore(root)
        val activeName = "book__ib-b0-s3-g7.pdf"
        val active = File(File(File(root, documentId), "active"), activeName)
        requireNotNull(active.parentFile).mkdirs()
        active.writeBytes("broker PDF".toByteArray())
        val state = HandoffState(
            documentId = documentId,
            originalFileName = "book.pdf",
            activeRevisions = RevisionPair(boox = 0, supernote = 3),
            sourceGeneration = 7,
            brokerEventId = "broker-delivery-7",
            activeFileName = activeName,
            installedBrokerSha256 = sha256Hex(active),
        )
        File(root, documentId).resolve(".inkbridge-state.json")
            .writeText(state.toJson().toString(2))
        return Triple(store, state, active)
    }

    private class FakeConverter : BooxManifestConverter {
        var manifestCalls = 0
        var baselineSourceFileName: String? = null

        override fun buildBaseline(pdf: File, sourceFileName: String): ByteArray {
            baselineSourceFileName = sourceFileName
            return BASELINE
        }

        override fun buildManifest(pdf: File, baseline: ByteArray): ByteArray {
            assertArrayEquals(BASELINE, baseline)
            manifestCalls += 1
            return manifest(sha256Hex(pdf))
        }

        companion object {
            val BASELINE: ByteArray = JSONObject()
                .put("schemaVersion", 1)
                .put("sourceFileName", "book.pdf")
                .put("pageCount", 1)
                .put("pdfSha256", sha256Hex("broker PDF".toByteArray()))
                .put("strokes", org.json.JSONArray())
                .toString()
                .toByteArray()

            fun manifest(pdfSha256: String): ByteArray = JSONObject()
                .put("schemaVersion", 1)
                .put(
                    "document",
                    JSONObject()
                        .put("sourceFileName", "book.pdf")
                        .put("pdfSha256", pdfSha256),
                )
                .put("operations", org.json.JSONArray().put(JSONObject().put("type", "upsert_stroke")))
                .toString()
                .toByteArray()
        }
    }
}

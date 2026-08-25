package dev.inkbridge.boox

import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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
    fun preparing_current_baseline_retires_only_obsolete_baseline_snapshots() {
        val root = Files.createTempDirectory("inkbridge-compact-baseline-retirement").toFile()
        val (store, state, _) = installedState(root)
        val documentRoot = store.documentDirectory(documentId)
        val obsolete = File(documentRoot, ".inkbridge-baseline-${"0".repeat(16)}.json")
            .apply { writeText("obsolete") }
        val unrelated = File(documentRoot, ".inkbridge-baseline-not-a-hash.json")
            .apply { writeText("preserve") }
        val finalizer = CompactBooxFinalizer(store, FakeConverter())

        val current = finalizer.prepareBaseline(state)

        assertTrue(current.isFile)
        assertFalse(obsolete.exists())
        assertTrue(unrelated.isFile)

        val laterObsolete = File(documentRoot, ".inkbridge-baseline-${"1".repeat(16)}.json")
            .apply { writeText("obsolete after restart") }
        assertEquals(current, finalizer.prepareBaseline(state))
        assertFalse(laterObsolete.exists())
        assertTrue(current.isFile)
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
        val outgoing = store.outgoingDirectory(documentId).listFiles().orEmpty()
        assertEquals(1, outgoing.size)
        assertTrue(outgoing.single().name.endsWith(".operations.json"))
        assertFalse(File(outgoing.single().parentFile, outgoing.single().name + ".inkbridge.json").exists())
    }

    @Test
    fun state_commit_before_descriptor_is_recovered_without_duplicate_conversion() {
        val root = Files.createTempDirectory("inkbridge-compact-state-crash").toFile()
        val (store, state, active) = installedState(root)
        val converter = FakeConverter()
        val finalizer = CompactBooxFinalizer(store, converter)
        finalizer.prepareBaseline(state)
        active.writeBytes("NeoReader edit".toByteArray())
        store.afterCompactStateCommitForTest = { error("simulated process death") }

        assertThrows(IllegalStateException::class.java) {
            finalizer.finalize(documentId)
        }

        val committed = requireNotNull(store.state(documentId))
        assertEquals(sha256Hex(active), committed.finalizedLocalSha256)
        val outgoing = store.outgoingDirectory(documentId)
        val manifest = outgoing.listFiles().orEmpty().single { it.name.endsWith(".operations.json") }
        val descriptor = File(outgoing, manifest.name + ".inkbridge.json")
        assertFalse(descriptor.exists())

        assertTrue(active.delete())
        store.afterCompactStateCommitForTest = null
        val recovered = finalizer.finalize(documentId) as CompactFinalizeResult.AlreadyFinalized

        assertEquals(manifest, recovered.manifest)
        assertTrue(descriptor.isFile)
        assertEquals(1, converter.manifestCalls)
    }

    @Test
    fun oversized_native_manifest_falls_back_before_publication_or_state_commit() {
        val root = Files.createTempDirectory("inkbridge-compact-oversized").toFile()
        val (store, state, active) = installedState(root)
        val oversizedManifest = FakeConverter.manifest(sha256Hex("edited PDF".toByteArray()))
        assertTrue(oversizedManifest.size > FakeConverter.BASELINE.size)
        val converter = object : BooxManifestConverter {
            override fun buildBaseline(pdf: File, sourceFileName: String) = FakeConverter.BASELINE
            override fun buildManifest(pdf: File, baseline: ByteArray) = oversizedManifest
        }
        val finalizer = CompactBooxFinalizer(
            store,
            converter,
            maxCompactJsonBytes = FakeConverter.BASELINE.size,
        )
        finalizer.prepareBaseline(state)
        active.writeBytes("edited PDF".toByteArray())

        val result = finalizer.finalize(documentId)

        assertTrue(result is CompactFinalizeResult.FullPdfFallbackRequired)
        assertTrue(store.outgoingDirectory(documentId).listFiles().orEmpty().isEmpty())
        assertEquals(null, store.state(documentId)?.finalizedLocalSha256)
    }

    @Test
    fun compact_entry_point_preserves_full_pdf_finalization_recovery() {
        val root = Files.createTempDirectory("inkbridge-full-fallback-recovery").toFile()
        val (store, _, active) = installedState(root)
        active.writeBytes("full PDF fallback edit".toByteArray())
        val full = store.finalize(documentId) as FinalizeResult.Finalized
        assertTrue(full.descriptor.delete())

        val recovered = CompactBooxFinalizer(store, FakeConverter()).finalize(documentId)

        assertTrue(recovered is CompactFinalizeResult.AlreadyFinalized)
        assertTrue(full.pdf.isFile)
        assertTrue(full.descriptor.isFile)
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

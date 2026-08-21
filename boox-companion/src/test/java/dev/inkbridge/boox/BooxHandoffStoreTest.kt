package dev.inkbridge.boox

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

class BooxHandoffStoreTest {
    @get:Rule
    val temporary = TemporaryFolder()

    private val documentId = "inkbridge-doc-v1-" + "a".repeat(64)

    @Test
    fun install_createsOneRevisionedActiveView_andDuplicateIsIdempotent() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val delivery = delivery(root, "event-1", RevisionPair(0, 1), 10, "pdf-one".toByteArray())

        val first = store.install(delivery) as InstallResult.Installed
        assertTrue(first.activeFile.isFile)
        assertEquals("pdf-one", first.activeFile.readText())
        assertEquals(1, first.activeFile.parentFile!!.listFiles()!!.size)

        val duplicate = store.install(delivery)
        assertTrue(duplicate is InstallResult.Duplicate)
        assertEquals(1, first.activeFile.parentFile!!.listFiles()!!.size)
        assertEquals(listOf("event-1"), store.state(documentId)!!.processedEventIds)
    }

    @Test
    fun install_rejectsStaleOrDivergentRevision() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        store.install(delivery(root, "event-1", RevisionPair(1, 1), 10, "one".toByteArray()))

        val stale = delivery(root, "event-stale", RevisionPair(0, 2), 11, "two".toByteArray())
        val error = runCatching { store.install(stale) }.exceptionOrNull()
        assertTrue(error!!.message!!.contains("stale or conflicts"))
        assertEquals(RevisionPair(1, 1), store.state(documentId)!!.activeRevisions)
    }

    @Test
    fun install_rejectsHashMismatchAndUnsafeFileName() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val descriptor = delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray())
        val json = JSONObject(descriptor.readText()).put("contentSha256", "b".repeat(64))
        descriptor.writeText(json.toString())
        assertTrue(runCatching { store.install(descriptor) }.exceptionOrNull()!!.message!!.contains("hash"))

        json.put("pdfFileName", "../escape.pdf")
        descriptor.writeText(json.toString())
        assertTrue(runCatching { store.install(descriptor) }.exceptionOrNull()!!.message!!.contains("file name"))
    }

    @Test
    fun unfinalizedNeoReaderChangeBlocksNewBrokerView() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val first = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        first.activeFile.appendText("-edited")

        val next = delivery(root, "event-2", RevisionPair(0, 2), 11, "two".toByteArray())
        val error = runCatching { store.install(next) }.exceptionOrNull()
        assertTrue(error!!.message!!.contains("Finalize"))
        assertTrue(first.activeFile.isFile)
    }

    @Test
    fun finalizedChangeRequiresBrokerAcknowledgementBeforeRetirement() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val first = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        first.activeFile.appendText("-edited")
        assertTrue(store.finalize(documentId) is FinalizeResult.Finalized)

        val notAccepted = delivery(root, "event-2", RevisionPair(0, 2), 11, "two".toByteArray())
        val error = runCatching { store.install(notAccepted) }.exceptionOrNull()
        assertTrue(error!!.message!!.contains("has not accepted"))

        val accepted = delivery(root, "event-3", RevisionPair(1, 2), 12, "three".toByteArray())
        val installed = store.install(accepted) as InstallResult.Installed
        assertEquals(RevisionPair(1, 2), installed.state.activeRevisions)
        assertEquals(1, installed.activeFile.parentFile!!.listFiles()!!.size)
        assertFalse(first.activeFile.exists())
    }

    @Test
    fun finalize_isIdempotentAndEmitsBrokerStorageEvent() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-1", RevisionPair(2, 4), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        assertEquals(FinalizeResult.NoChanges, store.finalize(documentId))

        installed.activeFile.appendText("-neo-reader-ink")
        val finalized = store.finalize(documentId) as FinalizeResult.Finalized
        assertEquals(installed.activeFile.readBytes().toList(), finalized.pdf.readBytes().toList())
        val event = JSONObject(finalized.descriptor.readText())
        assertEquals(documentId, event.getString("documentId"))
        assertEquals("boox", event.getString("source"))
        assertEquals(3, event.getLong("sourceRevision"))
        assertEquals(2, event.getJSONObject("basedOn").getLong("boox"))
        assertEquals(4, event.getJSONObject("basedOn").getLong("supernote"))
        assertEquals(sha256Hex(finalized.pdf.readBytes()), event.getString("contentSha256"))

        val again = store.finalize(documentId)
        assertTrue(again is FinalizeResult.AlreadyFinalized)
        assertEquals(2, finalized.pdf.parentFile!!.listFiles()!!.size)
    }

    @Test
    fun interruptedInstall_beforeReplacementPublication_keepsPredecessorRecoverable() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val first = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        val secondBytes = "two".toByteArray()
        val next = nextState(first, "event-2", RevisionPair(0, 2), 11, secondBytes)
        val documentRoot = File(root, documentId)
        writeIntent(documentRoot, first, next)

        assertEquals(first.state, store.state(documentId))
        assertTrue(first.activeFile.isFile)
        assertFalse(File(documentRoot, ".inkbridge-install.json").exists())
    }

    @Test
    fun interruptedInstall_afterReplacementPublication_commitsStateAndRetiresPredecessor() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val first = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        val secondBytes = "two".toByteArray()
        val descriptor = delivery(root, "event-2", RevisionPair(0, 2), 11, secondBytes)
        val next = nextState(first, "event-2", RevisionPair(0, 2), 11, secondBytes)
        val documentRoot = File(root, documentId)
        File(File(documentRoot, "active"), next.activeFileName).writeBytes(secondBytes)
        writeIntent(documentRoot, first, next)

        assertTrue(store.install(descriptor) is InstallResult.Duplicate)
        assertEquals(next, store.state(documentId))
        assertFalse(first.activeFile.exists())
        assertTrue(File(File(documentRoot, ".retired"), first.activeFile.name).isFile)
        assertFalse(File(documentRoot, ".inkbridge-install.json").exists())
    }

    @Test
    fun interruptedInstall_afterStateCommit_retiresPredecessorAndClearsIntent() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val first = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        val secondBytes = "two".toByteArray()
        val next = nextState(first, "event-2", RevisionPair(0, 2), 11, secondBytes)
        val documentRoot = File(root, documentId)
        File(File(documentRoot, "active"), next.activeFileName).writeBytes(secondBytes)
        writeIntent(documentRoot, first, next)
        File(documentRoot, ".inkbridge-state.json").writeText(next.toJson().toString(2))

        assertEquals(next, store.state(documentId))
        assertFalse(first.activeFile.exists())
        assertTrue(File(File(documentRoot, ".retired"), first.activeFile.name).isFile)
        assertFalse(File(documentRoot, ".inkbridge-install.json").exists())
    }

    @Test
    fun sameRevisionWithDifferentBytesIsRejected() {
        val state = state(RevisionPair(1, 2), "a".repeat(64))
        val different = brokerDelivery("other", RevisionPair(1, 2), 11, "b".repeat(64))
        val result = HandoffPolicy.decideInstall(state, different, state.installedBrokerSha256)
        assertTrue(result is InstallDecision.Reject)
    }


    @Test
    fun rustBrokerDeliveryDescriptorContract_parsesSafeUnicodeDocumentName() {
        val parsed = BrokerDelivery.fromJson(
            JSONObject(
                """
                {
                  "schemaVersion": 1,
                  "producer": "inkbridge-broker",
                  "eventId": "broker-event-19",
                  "documentId": "$documentId",
                  "originalFileName": "מסמך לדוגמה.pdf",
                  "sourceRevisions": {"boox": 2, "supernote": 4},
                  "sourceGeneration": 19,
                  "contentSha256": "${"b".repeat(64)}",
                  "pdfFileName": "broker-b00000000000000000002-s00000000000000000004-g00000000000000000019-bbbbbbbbbbbb.pdf"
                }
                """.trimIndent(),
            ),
        )

        assertEquals("broker-event-19", parsed.eventId)
        assertEquals("מסמך לדוגמה.pdf", parsed.originalFileName)
        assertEquals(RevisionPair(2, 4), parsed.sourceRevisions)
        assertEquals(19, parsed.sourceGeneration)
    }
    private fun nextState(
        first: InstallResult.Installed,
        eventId: String,
        revisions: RevisionPair,
        generation: Long,
        bytes: ByteArray,
    ) = HandoffState(
        documentId = documentId,
        originalFileName = "Example.pdf",
        activeRevisions = revisions,
        sourceGeneration = generation,
        brokerEventId = eventId,
        activeFileName = "Example__ib-b" + revisions.boox +
            "-s" + revisions.supernote + "-g" + generation + ".pdf",
        installedBrokerSha256 = sha256Hex(bytes),
        processedEventIds = first.state.processedEventIds + eventId,
    )

    private fun writeIntent(
        documentRoot: File,
        first: InstallResult.Installed,
        next: HandoffState,
    ) {
        File(documentRoot, ".inkbridge-install.json").writeText(
            InstallIntent(
                previousActiveFileName = first.activeFile.name,
                nextState = next,
            ).toJson().toString(2),
        )
    }

    private fun delivery(
        root: File,
        eventId: String,
        revisions: RevisionPair,
        generation: Long,
        bytes: ByteArray,
    ): File {
        val incoming = File(File(root, documentId), "incoming").also(File::mkdirs)
        val pdfName = "$eventId.pdf"
        File(incoming, pdfName).writeBytes(bytes)
        val descriptor = File(incoming, "$eventId.inkbridge.json")
        descriptor.writeText(
            brokerDelivery(eventId, revisions, generation, sha256Hex(bytes), pdfName).toJson().toString(2),
        )
        return descriptor
    }

    private fun brokerDelivery(
        eventId: String,
        revisions: RevisionPair,
        generation: Long,
        hash: String,
        pdfName: String = "incoming.pdf",
    ) = BrokerDelivery(
        schemaVersion = 1,
        producer = BROKER_PRODUCER,
        eventId = eventId,
        documentId = documentId,
        originalFileName = "Example.pdf",
        sourceRevisions = revisions,
        sourceGeneration = generation,
        contentSha256 = hash,
        pdfFileName = pdfName,
    )

    private fun state(revisions: RevisionPair, hash: String) = HandoffState(
        documentId = documentId,
        originalFileName = "Example.pdf",
        activeRevisions = revisions,
        sourceGeneration = 10,
        brokerEventId = "event-1",
        activeFileName = "active.pdf",
        installedBrokerSha256 = hash,
        processedEventIds = listOf("event-1"),
    )
}

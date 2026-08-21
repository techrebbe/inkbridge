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
    fun activeStates_listsEveryConfiguredDocumentForUserSelection() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val secondDocumentId = "inkbridge-doc-v1-" + "b".repeat(64)
        store.install(
            delivery(root, "event-a", RevisionPair(0, 1), 10, "one".toByteArray()),
        )
        store.install(
            delivery(
                root,
                "event-b",
                RevisionPair(0, 1),
                11,
                "two".toByteArray(),
                targetDocumentId = secondDocumentId,
            ),
        )

        assertEquals(
            listOf(documentId, secondDocumentId),
            store.activeStates().map(HandoffState::documentId),
        )
    }

    @Test
    fun activeDocumentCatalog_isolatesMalformedStateToItsDocument() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val secondDocumentId = "inkbridge-doc-v1-" + "b".repeat(64)
        store.install(
            delivery(root, "event-a", RevisionPair(0, 1), 10, "one".toByteArray()),
        )
        store.install(
            delivery(
                root,
                "event-b",
                RevisionPair(0, 1),
                11,
                "two".toByteArray(),
                targetDocumentId = secondDocumentId,
            ),
        )
        File(File(root, documentId), ".inkbridge-state.json").writeText("{")

        val catalog = store.activeDocumentCatalog()

        assertEquals(listOf(secondDocumentId), catalog.states.map(HandoffState::documentId))
        assertEquals(listOf(documentId), catalog.failures.map(DocumentRecoveryFailure::documentId))
        assertEquals(listOf(secondDocumentId), store.activeStates().map(HandoffState::documentId))
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
    fun install_preservesEditFlushedWhileReplacementIsCopied() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val first = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        var injected = false
        store.beforeInstallCommitForTest = { previous ->
            if (!injected) {
                injected = true
                requireNotNull(previous).appendText("-late-neo-reader-edit")
            }
        }
        val next = delivery(root, "event-2", RevisionPair(0, 2), 11, "two".toByteArray())

        val error = runCatching { store.install(next) }.exceptionOrNull()

        assertTrue(error!!.message!!.contains("edits were preserved"))
        assertEquals("one-late-neo-reader-edit", first.activeFile.readText())
        val state = store.state(documentId)!!
        assertEquals(RevisionPair(0, 1), state.activeRevisions)
        assertEquals(sha256Hex(first.activeFile), state.finalizedLocalSha256)
        assertFalse(File(File(root, documentId), ".inkbridge-install.json").exists())
        assertEquals(listOf(first.activeFile.name), first.activeFile.parentFile!!.listFiles()!!.map(File::getName))
        val finalizedPdf = File(File(root, documentId), "outgoing")
            .listFiles().orEmpty().single { it.name.endsWith(".pdf") }
        assertEquals(first.activeFile.readBytes().toList(), finalizedPdf.readBytes().toList())
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
    fun acknowledgingDeliveryPreservesPostFinalizationEditsAsConflictArtifact() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val first = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        first.activeFile.appendText("-first-edit")
        val finalized = store.finalize(documentId) as FinalizeResult.Finalized
        first.activeFile.appendText("-second-edit")
        val postFinalizationBytes = first.activeFile.readBytes()
        val postFinalizationHash = sha256Hex(postFinalizationBytes)
        val acknowledging = delivery(
            root,
            "event-ack",
            RevisionPair(1, 2),
            11,
            "acknowledged-view".toByteArray(),
        )

        val installed = store.install(acknowledging) as InstallResult.Installed

        assertEquals("acknowledged-view", installed.activeFile.readText())
        assertEquals(RevisionPair(1, 2), installed.state.activeRevisions)
        assertEquals(null, installed.state.finalizedLocalSha256)
        val outgoing = finalized.pdf.parentFile!!
        val descriptors = outgoing.listFiles().orEmpty()
            .filter { it.name.endsWith(".inkbridge.json") }
            .map { it to JSONObject(it.readText()) }
        assertEquals(2, descriptors.size)
        val conflict = descriptors.single { (_, value) ->
            value.getString("contentSha256") == postFinalizationHash
        }
        assertEquals(0, conflict.second.getJSONObject("basedOn").getLong("boox"))
        assertEquals(1, conflict.second.getJSONObject("basedOn").getLong("supernote"))
        assertEquals(1, conflict.second.getLong("sourceRevision"))
        val conflictPdfName = conflict.first.name.removeSuffix(".inkbridge.json")
        assertEquals(
            postFinalizationBytes.toList(),
            File(outgoing, conflictPdfName).readBytes().toList(),
        )
        assertTrue(finalized.pdf.isFile)
        assertTrue(finalized.descriptor.isFile)
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
    fun finalize_rejectsSecondEditUntilPriorFinalizationIsAcknowledged() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        installed.activeFile.appendText("-first-edit")
        val first = store.finalize(documentId) as FinalizeResult.Finalized
        val descriptorBytes = first.descriptor.readBytes()
        assertTrue(first.descriptor.delete())
        installed.activeFile.appendText("-second-edit")

        val error = runCatching { store.finalize(documentId) }.exceptionOrNull()

        assertTrue(error!!.message!!.contains("acknowledged"))
        assertTrue(first.pdf.isFile)
        assertTrue(first.descriptor.isFile)
        assertTrue(descriptorBytes.contentEquals(first.descriptor.readBytes()))
        assertEquals(2, first.pdf.parentFile!!.listFiles()!!.size)
        assertEquals("one-first-edit-second-edit", installed.activeFile.readText())
    }

    @Test
    fun finalizedEventIdentityIncludesTheRevisionFrontier() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val firstInstalled = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "broker-one".toByteArray()),
        ) as InstallResult.Installed
        firstInstalled.activeFile.writeText("repeated-finalized-content")
        val first = store.finalize(documentId) as FinalizeResult.Finalized
        val firstEventId = JSONObject(first.descriptor.readText()).getString("eventId")

        val acknowledged = store.install(
            delivery(root, "event-2", RevisionPair(1, 2), 11, "broker-two".toByteArray()),
        ) as InstallResult.Installed
        acknowledged.activeFile.writeText("repeated-finalized-content")
        val second = store.finalize(documentId) as FinalizeResult.Finalized
        val secondEventId = JSONObject(second.descriptor.readText()).getString("eventId")

        assertTrue(firstEventId != secondEventId)
        assertEquals(
            JSONObject(first.descriptor.readText()).getString("contentSha256"),
            JSONObject(second.descriptor.readText()).getString("contentSha256"),
        )
    }

    @Test
    fun interruptedFinalize_beforeArtifactPublication_recoversPairAndState() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        installed.activeFile.appendText("-edited")
        val intent = finalizeIntent(installed, sha256Hex(installed.activeFile))
        val documentRoot = File(root, documentId)
        writeFinalizeIntent(documentRoot, intent)

        assertEquals(intent.nextState, store.state(documentId))

        val output = File(File(documentRoot, "outgoing"), intent.nextState.finalizedOutputFileName!!)
        assertTrue(output.isFile)
        assertTrue(File(output.parentFile, output.name + ".inkbridge.json").isFile)
        assertEquals(installed.activeFile.readBytes().toList(), output.readBytes().toList())
        assertFalse(File(documentRoot, ".inkbridge-finalize.json").exists())
    }

    @Test
    fun interruptedFinalize_duringArtifactCopy_removesStagedFilesBeforeRetrying() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        installed.activeFile.appendText("-edited")
        val intent = finalizeIntent(installed, sha256Hex(installed.activeFile))
        val documentRoot = File(root, documentId)
        val outgoing = File(documentRoot, "outgoing").also(File::mkdirs)
        val outputName = intent.nextState.finalizedOutputFileName!!
        val stagedPdf = File(outgoing, ".$outputName.123.tmp").apply { writeText("partial PDF") }
        val stagedDescriptor = File(outgoing, ".$outputName.inkbridge.json.124.tmp").apply {
            writeText("partial descriptor")
        }
        writeFinalizeIntent(documentRoot, intent)

        assertEquals(intent.nextState, store.state(documentId))
        assertFalse(stagedPdf.exists())
        assertFalse(stagedDescriptor.exists())
        assertTrue(File(outgoing, outputName).isFile)
        assertTrue(File(outgoing, "$outputName.inkbridge.json").isFile)
        assertFalse(File(documentRoot, ".inkbridge-finalize.json").exists())
    }

    @Test
    fun interruptedFinalize_afterArtifactPublication_commitsPendingStateWithoutDuplicateEvent() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        installed.activeFile.appendText("-edited")
        val finalized = store.finalize(documentId) as FinalizeResult.Finalized
        val documentRoot = File(root, documentId)
        File(documentRoot, ".inkbridge-state.json").writeText(installed.state.toJson().toString(2))
        writeFinalizeIntent(
            documentRoot,
            FinalizeIntent(previousState = installed.state, nextState = finalized.state),
        )

        assertEquals(finalized.state, store.state(documentId))
        assertEquals(2, finalized.pdf.parentFile!!.listFiles()!!.size)
        assertTrue(finalized.pdf.isFile)
        assertTrue(finalized.descriptor.isFile)
        assertFalse(File(documentRoot, ".inkbridge-finalize.json").exists())
        assertTrue(store.finalize(documentId) is FinalizeResult.AlreadyFinalized)
    }

    @Test
    fun interruptedFinalize_withoutPublishedSnapshot_restartsAfterTheActivePdfChanges() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        installed.activeFile.appendText("-first-edit")
        val documentRoot = File(root, documentId)
        writeFinalizeIntent(documentRoot, finalizeIntent(installed, sha256Hex(installed.activeFile)))
        installed.activeFile.appendText("-second-edit")

        assertEquals(installed.state, store.state(documentId))
        assertFalse(File(documentRoot, ".inkbridge-finalize.json").exists())
        val finalized = store.finalize(documentId) as FinalizeResult.Finalized
        assertEquals(sha256Hex(installed.activeFile), finalized.state.finalizedLocalSha256)
    }

    @Test
    fun malformedCurrentState_restoresValidPreviousAndPreservesCorruptBytes() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        val documentRoot = File(root, documentId)
        val current = File(documentRoot, ".inkbridge-state.json")
        val previous = File(documentRoot, ".inkbridge-state.previous")
        previous.writeBytes(current.readBytes())
        current.writeText("{")

        assertEquals(installed.state, store.state(documentId))
        assertEquals(installed.state, HandoffState.fromJson(JSONObject(current.readText())))
        assertFalse(previous.exists())
        val preserved = documentRoot.listFiles().orEmpty().single {
            it.name.startsWith(".inkbridge-state.json.corrupt-")
        }
        assertEquals("{", preserved.readText())
    }

    @Test
    fun missingCommittedStateWithActivePdfFailsClosed() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        val documentRoot = File(root, documentId)
        assertTrue(File(documentRoot, ".inkbridge-state.json").delete())
        val next = delivery(root, "event-2", RevisionPair(0, 2), 11, "two".toByteArray())

        assertEquals(null, store.findNextDescriptor())
        val error = runCatching { store.install(next) }.exceptionOrNull()

        assertTrue(error!!.message!!.contains("state is missing"))
        assertTrue(installed.activeFile.isFile)
        assertEquals("one", installed.activeFile.readText())
        assertEquals(1, installed.activeFile.parentFile!!.listFiles()!!.size)
    }

    @Test
    fun missingStateIsRecoveredFromAnInterruptedInitialInstallBeforeFailClosedGuard() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val documentRoot = File(root, documentId)
        val bytes = "one".toByteArray()
        val next = HandoffState(
            documentId = documentId,
            originalFileName = "Example.pdf",
            activeRevisions = RevisionPair(0, 1),
            sourceGeneration = 10,
            brokerEventId = "event-1",
            activeFileName = "Example__ib-b0-s1-g10.pdf",
            installedBrokerSha256 = sha256Hex(bytes),
            processedEventIds = listOf("event-1"),
        )
        val active = File(File(documentRoot, "active").also(File::mkdirs), next.activeFileName)
        active.writeBytes(bytes)
        File(documentRoot, ".inkbridge-install.json").writeText(
            InstallIntent(previousState = null, previousActiveSha256 = null, nextState = next).toJson().toString(2),
        )

        assertEquals(next, store.state(documentId))
        assertTrue(active.isFile)
        assertFalse(File(documentRoot, ".inkbridge-install.json").exists())
    }

    @Test
    fun interruptedInitialInstall_duringReplacementCopy_removesStagedFileAndCanRetry() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val documentRoot = File(root, documentId)
        val bytes = "one".toByteArray()
        val next = HandoffState(
            documentId = documentId,
            originalFileName = "Example.pdf",
            activeRevisions = RevisionPair(0, 1),
            sourceGeneration = 10,
            brokerEventId = "event-1",
            activeFileName = "Example__ib-b0-s1-g10.pdf",
            installedBrokerSha256 = sha256Hex(bytes),
            processedEventIds = listOf("event-1"),
        )
        val active = File(documentRoot, "active").also(File::mkdirs)
        val staged = File(active, ".${next.activeFileName}.123.tmp").apply { writeText("partial") }
        File(documentRoot, ".inkbridge-install.json").writeText(
            InstallIntent(previousState = null, previousActiveSha256 = null, nextState = next).toJson().toString(2),
        )

        assertEquals(null, store.state(documentId))
        assertFalse(staged.exists())
        assertFalse(File(documentRoot, ".inkbridge-install.json").exists())

        val installed = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, bytes),
        ) as InstallResult.Installed
        assertEquals("one", installed.activeFile.readText())
        assertEquals(next, installed.state)
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
    fun interruptedInstall_afterStateCommit_preservesEditedPredecessorBeforeRetirement() {
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
        first.activeFile.appendText("-late-neoreader-edit")
        val editedHash = sha256Hex(first.activeFile)
        val outputName = first.activeFile.nameWithoutExtension +
            "__boox-finalized-g1-" + editedHash.take(12) + ".pdf"
        val outgoing = File(documentRoot, "outgoing").also(File::mkdirs)
        val stagedPdf = File(outgoing, ".$outputName.123.tmp").apply { writeText("partial PDF") }
        val stagedDescriptor = File(outgoing, ".$outputName.inkbridge.json.124.tmp").apply {
            writeText("partial descriptor")
        }

        assertEquals(next, store.state(documentId))
        assertFalse(first.activeFile.exists())
        assertTrue(File(File(documentRoot, ".retired"), first.activeFile.name).isFile)
        assertFalse(stagedPdf.exists())
        assertFalse(stagedDescriptor.exists())
        val preserved = outgoing.listFiles().orEmpty().single { it.extension == "pdf" }
        assertEquals("one-late-neoreader-edit", preserved.readText())
        assertTrue(File(outgoing, preserved.name + ".inkbridge.json").isFile)
        assertFalse(File(documentRoot, ".inkbridge-install.json").exists())
    }

    @Test
    fun interruptedInstall_preservesFirstEditFlushedAtRetirementBoundary() {
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
        var injectLateEdit = true
        store.beforePreservedDescriptorForTest = { predecessor ->
            if (injectLateEdit) {
                injectLateEdit = false
                predecessor.appendText("-first-late-edit")
            }
        }

        val error = runCatching { store.state(documentId) }.exceptionOrNull()

        assertTrue(error!!.message!!.contains("before retirement"))
        assertTrue(first.activeFile.isFile)
        assertTrue(File(documentRoot, ".inkbridge-install.json").isFile)
        val outgoing = File(documentRoot, "outgoing")
        assertFalse(outgoing.listFiles().orEmpty().any { it.extension == "pdf" })

        store.beforePreservedDescriptorForTest = null
        assertEquals(next, store.state(documentId))
        val preserved = outgoing.listFiles().orEmpty().single { it.extension == "pdf" }
        assertEquals("one-first-late-edit", preserved.readText())
        assertTrue(File(outgoing, preserved.name + ".inkbridge.json").isFile)
        assertFalse(first.activeFile.exists())
    }

    @Test
    fun interruptedInstall_afterEditedPredecessorRetirement_completesOutgoingPair() {
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
        first.activeFile.appendText("-late-edit")
        val editedHash = sha256Hex(first.activeFile)
        val outputName = first.activeFile.nameWithoutExtension +
            "__boox-finalized-g1-" + editedHash.take(12) + ".pdf"
        val outgoing = File(documentRoot, "outgoing").also(File::mkdirs)
        val output = File(outgoing, outputName).apply { writeBytes(first.activeFile.readBytes()) }
        val retired = File(File(documentRoot, ".retired").also(File::mkdirs), first.activeFile.name)
        assertTrue(first.activeFile.renameTo(retired))

        assertEquals(next, store.state(documentId))
        assertFalse(first.activeFile.exists())
        assertEquals("one-late-edit", retired.readText())
        assertEquals("one-late-edit", output.readText())
        assertTrue(File(outgoing, "$outputName.inkbridge.json").isFile)
        assertFalse(File(documentRoot, ".inkbridge-install.json").exists())
    }

    @Test
    fun interruptedInstall_discardsSnapshotWhenPredecessorChangesBeforeDescriptorPublication() {
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
        first.activeFile.appendText("-late-edit")
        var injectNewerEdit = true
        store.beforePreservedDescriptorForTest = { predecessor ->
            if (injectNewerEdit) {
                injectNewerEdit = false
                predecessor.appendText("-newer-edit")
            }
        }

        val error = runCatching { store.state(documentId) }.exceptionOrNull()

        assertTrue(error!!.message!!.contains("continued changing"))
        assertTrue(first.activeFile.isFile)
        assertTrue(File(documentRoot, ".inkbridge-install.json").isFile)
        val outgoing = File(documentRoot, "outgoing")
        assertFalse(outgoing.listFiles().orEmpty().any { it.extension == "pdf" })
        assertFalse(outgoing.listFiles().orEmpty().any { it.name.endsWith(".inkbridge.json") })

        store.beforePreservedDescriptorForTest = null
        assertEquals(next, store.state(documentId))
        val preserved = outgoing.listFiles().orEmpty().single { it.extension == "pdf" }
        assertEquals("one-late-edit-newer-edit", preserved.readText())
        assertTrue(File(outgoing, preserved.name + ".inkbridge.json").isFile)
        assertFalse(first.activeFile.exists())
        assertEquals(
            "one-late-edit-newer-edit",
            File(File(documentRoot, ".retired"), first.activeFile.name).readText(),
        )
    }

    @Test
    fun publication_rejectsBytesThatDoNotMatchTheInitialHash() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val source = File(root, "source.pdf").apply { writeText("changed-after-initial-hash") }
        val destination = File(root, "published.pdf")
        val initialHash = sha256Hex("original".toByteArray())

        val error = runCatching {
            store.publishFileOrVerify(source, destination, initialHash)
        }.exceptionOrNull()

        assertTrue(error!!.message!!.contains("changed while it was being copied"))
        assertFalse(destination.exists())
        assertFalse(root.listFiles().orEmpty().any { it.name.endsWith(".tmp") })
    }

    @Test
    fun descriptorQueue_skipsDocumentWithUnfinalizedEditAndFindsAnotherDocument() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val secondDocumentId = "inkbridge-doc-v1-" + "b".repeat(64)
        val blocked = store.install(
            delivery(root, "event-a-current", RevisionPair(0, 1), 10, "a-current".toByteArray()),
        ) as InstallResult.Installed
        store.install(
            delivery(
                root,
                "event-b-current",
                RevisionPair(0, 1),
                10,
                "b-current".toByteArray(),
                secondDocumentId,
            ),
        )
        blocked.activeFile.appendText("-unfinalized-edit")
        delivery(root, "event-a-next", RevisionPair(0, 2), 11, "a-next".toByteArray())
        val available = delivery(
            root,
            "event-b-next",
            RevisionPair(0, 2),
            11,
            "b-next".toByteArray(),
            secondDocumentId,
        )

        assertEquals(available.canonicalFile, store.findNextDescriptor()!!.canonicalFile)
    }

    @Test
    fun descriptorQueue_skipsExpiredEventsOutsideProcessedIdWindow() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        store.install(
            delivery(root, "event-current", RevisionPair(5, 5), 10, "current".toByteArray()),
        )
        delivery(root, "a-expired", RevisionPair(0, 1), 1, "expired".toByteArray())
        val next = delivery(root, "z-next", RevisionPair(5, 6), 11, "next".toByteArray())

        assertEquals(next.canonicalFile, store.findNextDescriptor()!!.canonicalFile)
    }

    @Test
    fun descriptorQueue_skipsIncomparableRevisionAheadOfValidDelivery() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        store.install(
            delivery(root, "event-current", RevisionPair(1, 1), 10, "current".toByteArray()),
        )
        delivery(root, "a-incomparable", RevisionPair(0, 2), 11, "conflict".toByteArray())
        delivery(root, "b-old-generation", RevisionPair(2, 2), 10, "old".toByteArray())
        val next = delivery(root, "z-next", RevisionPair(2, 2), 12, "next".toByteArray())

        assertEquals(next.canonicalFile, store.findNextDescriptor()!!.canonicalFile)
    }

    @Test
    fun descriptorQueue_skipsSameRevisionHashConflictAheadOfValidDelivery() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        store.install(
            delivery(root, "event-current", RevisionPair(1, 1), 10, "current".toByteArray()),
        )
        delivery(root, "a-conflict", RevisionPair(1, 1), 11, "conflict".toByteArray())
        val next = delivery(root, "z-next", RevisionPair(1, 2), 12, "next".toByteArray())

        assertEquals(next.canonicalFile, store.findNextDescriptor()!!.canonicalFile)
    }

    @Test
    fun descriptorQueue_allowsAcknowledgementThatPreservesPostFinalizationEdit() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-current", RevisionPair(1, 1), 10, "current".toByteArray()),
        ) as InstallResult.Installed
        installed.activeFile.appendText("-first-edit")
        assertTrue(store.finalize(documentId) is FinalizeResult.Finalized)
        installed.activeFile.appendText("-second-edit")
        val acknowledged = delivery(
            root,
            "event-acknowledged",
            RevisionPair(2, 2),
            12,
            "acknowledged".toByteArray(),
        )

        assertEquals(acknowledged.canonicalFile, store.findNextDescriptor()!!.canonicalFile)
        assertTrue(store.install(acknowledged) is InstallResult.Installed)
        val outgoingPdfs = File(File(root, documentId), "outgoing")
            .listFiles()
            .orEmpty()
            .filter { it.extension == "pdf" }
        assertEquals(2, outgoingPdfs.size)
        assertTrue(outgoingPdfs.any { it.readText() == "current-first-edit-second-edit" })
    }

    @Test
    fun descriptorQueue_skipsUnacknowledgedUpdateAfterFinalizedBooxEdit() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-current", RevisionPair(1, 1), 10, "current".toByteArray()),
        ) as InstallResult.Installed
        installed.activeFile.appendText("-edited")
        assertTrue(store.finalize(documentId) is FinalizeResult.Finalized)
        delivery(root, "a-unacknowledged", RevisionPair(1, 2), 11, "intermediate".toByteArray())
        val acknowledged = delivery(
            root,
            "z-acknowledged",
            RevisionPair(2, 2),
            12,
            "acknowledged".toByteArray(),
        )

        assertEquals(acknowledged.canonicalFile, store.findNextDescriptor()!!.canonicalFile)
        assertTrue(store.install(acknowledged) is InstallResult.Installed)
    }

    @Test
    fun missingCommittedActivePdf_isRecoveredBeforeInstallingNewerDelivery() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val first = store.install(
            delivery(root, "event-1", RevisionPair(0, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        assertTrue(first.activeFile.delete())
        val next = delivery(root, "event-2", RevisionPair(0, 2), 11, "two".toByteArray())

        assertEquals(next.canonicalFile, store.findNextDescriptor()!!.canonicalFile)
        assertTrue(first.activeFile.isFile)
        assertEquals("one", first.activeFile.readText())
        val installed = store.install(next) as InstallResult.Installed
        assertEquals("two", installed.activeFile.readText())
        assertEquals(RevisionPair(0, 2), installed.state.activeRevisions)
    }

    @Test
    fun missingFinalizedActivePdf_isRecoveredFromFinalizedOutgoingPdf() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-1", RevisionPair(1, 1), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        installed.activeFile.appendText("-finalized")
        store.finalize(documentId)
        assertTrue(installed.activeFile.delete())
        val staged = File(
            installed.activeFile.parentFile,
            ".${installed.activeFile.name}.123.tmp",
        ).apply { writeText("partial recovery") }

        store.state(documentId)

        assertFalse(staged.exists())
        assertTrue(installed.activeFile.isFile)
        assertEquals("one-finalized", installed.activeFile.readText())
        assertTrue(store.finalize(documentId) is FinalizeResult.AlreadyFinalized)
    }

    @Test
    fun finalize_recreatesMissingPublishedPairFromActivePdf() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val installed = store.install(
            delivery(root, "event-1", RevisionPair(2, 4), 10, "one".toByteArray()),
        ) as InstallResult.Installed
        installed.activeFile.appendText("-neo-reader-ink")
        val finalized = store.finalize(documentId) as FinalizeResult.Finalized
        val expectedPdf = finalized.pdf.readBytes()
        val expectedDescriptor = finalized.descriptor.readBytes()
        assertTrue(finalized.pdf.delete())
        assertTrue(finalized.descriptor.delete())

        val recovered = store.finalize(documentId)
        assertTrue(recovered is FinalizeResult.AlreadyFinalized)
        assertTrue(finalized.pdf.isFile)
        assertTrue(finalized.descriptor.isFile)
        assertTrue(expectedPdf.contentEquals(finalized.pdf.readBytes()))
        assertTrue(expectedDescriptor.contentEquals(finalized.descriptor.readBytes()))
    }

    @Test
    fun descriptorQueue_skipsMalformedAndIncompletePairs() {
        val root = temporary.newFolder("root")
        val store = BooxHandoffStore(root)
        val incoming = File(File(root, documentId), "incoming").also(File::mkdirs)
        File(incoming, "a-malformed.inkbridge.json").writeText("{not-json")
        File(incoming, "b-missing.inkbridge.json").writeText(
            brokerDelivery(
                "missing-pdf",
                RevisionPair(0, 1),
                1,
                "a".repeat(64),
                "missing.pdf",
            ).toJson().toString(2),
        )
        File(incoming, "c-corrupt.pdf").writeText("truncated")
        File(incoming, "c-corrupt.inkbridge.json").writeText(
            brokerDelivery(
                "corrupt-pdf",
                RevisionPair(0, 1),
                1,
                "b".repeat(64),
                "c-corrupt.pdf",
            ).toJson().toString(2),
        )
        val valid = delivery(root, "z-valid", RevisionPair(0, 1), 2, "valid".toByteArray())

        assertEquals(valid.canonicalFile, store.findNextDescriptor()!!.canonicalFile)
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

    private fun finalizeIntent(
        installed: InstallResult.Installed,
        contentHash: String,
    ): FinalizeIntent {
        val nextGeneration = installed.state.localGeneration + 1
        val outputName = installed.activeFile.nameWithoutExtension +
            "__boox-finalized-g" + nextGeneration + "-" + contentHash.take(12) + ".pdf"
        return FinalizeIntent(
            previousState = installed.state,
            nextState = installed.state.copy(
                finalizedLocalSha256 = contentHash,
                finalizedOutputFileName = outputName,
                localGeneration = nextGeneration,
            ),
        )
    }

    private fun writeFinalizeIntent(documentRoot: File, intent: FinalizeIntent) {
        File(documentRoot, ".inkbridge-finalize.json").writeText(intent.toJson().toString(2))
    }

    private fun writeIntent(
        documentRoot: File,
        first: InstallResult.Installed,
        next: HandoffState,
    ) {
        File(documentRoot, ".inkbridge-install.json").writeText(
            InstallIntent(
                previousState = first.state,
                previousActiveSha256 = sha256Hex(first.activeFile),
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
        targetDocumentId: String = documentId,
    ): File {
        val incoming = File(File(root, targetDocumentId), "incoming").also(File::mkdirs)
        val pdfName = "$eventId.pdf"
        File(incoming, pdfName).writeBytes(bytes)
        val descriptor = File(incoming, "$eventId.inkbridge.json")
        descriptor.writeText(
            brokerDelivery(
                eventId,
                revisions,
                generation,
                sha256Hex(bytes),
                pdfName,
                targetDocumentId,
            ).toJson().toString(2),
        )
        return descriptor
    }

    private fun brokerDelivery(
        eventId: String,
        revisions: RevisionPair,
        generation: Long,
        hash: String,
        pdfName: String = "incoming.pdf",
        targetDocumentId: String = documentId,
    ) = BrokerDelivery(
        schemaVersion = 1,
        producer = BROKER_PRODUCER,
        eventId = eventId,
        documentId = targetDocumentId,
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

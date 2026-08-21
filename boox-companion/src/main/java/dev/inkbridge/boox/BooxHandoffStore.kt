package dev.inkbridge.boox

import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.nio.channels.FileChannel
import java.nio.file.AccessDeniedException
import java.nio.file.Files
import java.nio.file.StandardOpenOption
import java.security.MessageDigest

sealed class InstallResult {
    data class Installed(val activeFile: File, val state: HandoffState) : InstallResult()
    data class Duplicate(val activeFile: File?) : InstallResult()
}

sealed class FinalizeResult {
    data object NoChanges : FinalizeResult()
    data class Finalized(val pdf: File, val descriptor: File, val state: HandoffState) : FinalizeResult()
    data class AlreadyFinalized(val pdf: File?) : FinalizeResult()
}

data class DocumentRecoveryFailure(val documentId: String, val message: String)

data class ActiveDocumentCatalog(
    val states: List<HandoffState>,
    val failures: List<DocumentRecoveryFailure>,
)

private data class FinalizationCommit(
    val pdf: File,
    val descriptor: File,
    val state: HandoffState,
)

class BooxHandoffStore(val root: File) {
    internal var beforeInstallCommitForTest: ((File?) -> Unit)? = null
    fun install(descriptorFile: File): InstallResult {
        val delivery = BrokerDelivery.fromJson(JSONObject(descriptorFile.readText()))
        val documentRoot = documentRoot(delivery.documentId)
        val incomingPdf = File(descriptorFile.parentFile, delivery.pdfFileName)
        require(incomingPdf.isFile) { "Incoming PDF is missing" }
        val incomingHash = sha256Hex(incomingPdf)
        require(incomingHash == delivery.contentSha256) { "Incoming PDF hash does not match its descriptor" }

        recover(documentRoot)
        var previous = readState(documentRoot)
        val previousActive = previous?.let { File(activeDir(documentRoot), it.activeFileName) }
        val activeHash = previousActive?.takeIf(File::isFile)?.let(::sha256Hex)
        if (
            previous != null && previousActive != null && activeHash != null &&
            shouldPreservePostFinalizationEdit(previous, delivery, activeHash)
        ) {
            previous = commitFinalization(documentRoot, previous, previousActive, activeHash).state
        }
        return when (val decision = HandoffPolicy.decideInstall(previous, delivery, activeHash)) {
            InstallDecision.Duplicate -> InstallResult.Duplicate(previousActive)
            is InstallDecision.Reject -> error(decision.reason)
            InstallDecision.Install -> installAccepted(
                delivery,
                incomingPdf,
                documentRoot,
                previous,
                previousActive,
                activeHash,
            )
        }
    }

    fun finalize(documentId: String): FinalizeResult {
        requireDocumentId(documentId)
        val documentRoot = documentRoot(documentId)
        recover(documentRoot)
        val state = readState(documentRoot) ?: error("No active InkBridge document")
        val active = File(activeDir(documentRoot), state.activeFileName)
        require(active.isFile) { "The active PDF is missing" }
        val currentHash = sha256Hex(active)
        val finalizedHash = state.finalizedLocalSha256
        if (finalizedHash != null) {
            val outputName = requireNotNull(state.finalizedOutputFileName)
            val output = File(outgoingDir(documentRoot), outputName)
            val recoverySource = when {
                currentHash == finalizedHash -> active
                output.isFile -> output
                else -> error(
                    "The pending finalized BOOX snapshot is missing; preserving the newer active edit",
                )
            }
            val artifacts = ensureFinalizedArtifacts(
                documentRoot,
                state,
                recoverySource,
                finalizedHash,
                outputName,
                state.localGeneration,
            )
            check(currentHash == finalizedHash) {
                "Wait for the previous finalized BOOX changes to be acknowledged before finalizing again"
            }
            return FinalizeResult.AlreadyFinalized(artifacts.first)
        }
        if (currentHash == state.installedBrokerSha256) return FinalizeResult.NoChanges

        val committed = commitFinalization(documentRoot, state, active, currentHash)
        return FinalizeResult.Finalized(committed.pdf, committed.descriptor, committed.state)
    }
    fun state(documentId: String): HandoffState? {
        requireDocumentId(documentId)
        val documentRoot = documentRoot(documentId)
        recover(documentRoot)
        return readState(documentRoot)
    }

    fun findNextDescriptor(): File? = root
        .listFiles()
        .orEmpty()
        .asSequence()
        .filter { it.isDirectory && it.name.startsWith("inkbridge-doc-v1-") }
        .flatMap { File(it, "incoming").listFiles().orEmpty().asSequence() }
        .filter { it.isFile && it.name.endsWith(".inkbridge.json") }
        .sortedBy { it.absolutePath }
        .firstOrNull { descriptor ->
            runCatching {
                val delivery = BrokerDelivery.fromJson(JSONObject(descriptor.readText()))
                val deliveryRoot = documentRoot(delivery.documentId)
                recover(deliveryRoot)
                val state = readState(deliveryRoot)
                val candidate = when {
                    state == null -> true
                    delivery.eventId in state.processedEventIds -> false
                    delivery.sourceRevisions == state.activeRevisions -> false
                    !delivery.sourceRevisions.strictlyDominates(state.activeRevisions) -> false
                    delivery.sourceGeneration <= state.sourceGeneration -> false
                    state.finalizedLocalSha256 != null &&
                        delivery.sourceRevisions.boox <= state.activeRevisions.boox -> false
                    else -> true
                }
                if (!candidate) return@runCatching false

                val incomingPdf = File(descriptor.parentFile, delivery.pdfFileName)
                require(incomingPdf.isFile) { "Incoming PDF is missing" }
                require(sha256Hex(incomingPdf) == delivery.contentSha256) {
                    "Incoming PDF hash does not match its descriptor"
                }
                true
            }.getOrDefault(false)
        }

    fun activeDocumentCatalog(): ActiveDocumentCatalog {
        val states = mutableListOf<HandoffState>()
        val failures = mutableListOf<DocumentRecoveryFailure>()
        root.listFiles()
            .orEmpty()
            .asSequence()
            .filter { it.isDirectory && it.name.startsWith("inkbridge-doc-v1-") }
            .forEach { documentRoot ->
                try {
                    recover(documentRoot)
                    readState(documentRoot)?.let(states::add)
                } catch (error: Exception) {
                    failures += DocumentRecoveryFailure(
                        documentId = documentRoot.name,
                        message = error.message ?: error.javaClass.simpleName,
                    )
                }
            }
        states.sortWith(compareBy<HandoffState> { it.originalFileName.lowercase() }.thenBy { it.documentId })
        failures.sortBy(DocumentRecoveryFailure::documentId)
        return ActiveDocumentCatalog(states, failures)
    }

    fun activeStates(): List<HandoffState> = activeDocumentCatalog().states

    fun findMostRecentState(): HandoffState? = activeStates().maxByOrNull { it.sourceGeneration }

    private fun shouldPreservePostFinalizationEdit(
        state: HandoffState,
        delivery: BrokerDelivery,
        activeHash: String,
    ): Boolean =
        state.finalizedLocalSha256 != null &&
            activeHash != state.installedBrokerSha256 &&
            activeHash != state.finalizedLocalSha256 &&
            delivery.eventId !in state.processedEventIds &&
            delivery.sourceRevisions.strictlyDominates(state.activeRevisions) &&
            delivery.sourceGeneration > state.sourceGeneration &&
            delivery.sourceRevisions.boox > state.activeRevisions.boox

    private fun commitFinalization(
        documentRoot: File,
        state: HandoffState,
        active: File,
        currentHash: String,
    ): FinalizationCommit {
        val nextLocalGeneration = state.localGeneration + 1
        val outputName = active.nameWithoutExtension +
            "__boox-finalized-g" + nextLocalGeneration + "-" + currentHash.take(12) + ".pdf"
        val next = state.copy(
            finalizedLocalSha256 = currentHash,
            finalizedOutputFileName = outputName,
            localGeneration = nextLocalGeneration,
        )
        writeFinalizeIntent(documentRoot, FinalizeIntent(previousState = state, nextState = next))
        val artifacts = ensureFinalizedArtifacts(
            documentRoot,
            state,
            active,
            currentHash,
            outputName,
            nextLocalGeneration,
        )
        writeState(documentRoot, next)
        clearFinalizeIntent(documentRoot)
        return FinalizationCommit(artifacts.first, artifacts.second, next)
    }

    private fun ensureFinalizedArtifacts(
        documentRoot: File,
        state: HandoffState,
        active: File,
        currentHash: String,
        outputName: String,
        localGeneration: Long,
    ): Pair<File, File> {
        requireSafeFileName(outputName, "finalized output file name")
        require(localGeneration >= 1) { "Invalid local generation" }
        val output = File(outgoingDir(documentRoot), outputName)
        publishFileOrVerify(active, output, currentHash)
        val eventIdentity = state.documentId + ":" +
            state.activeRevisions.boox + ":" + state.activeRevisions.supernote + ":" +
            (state.activeRevisions.boox + 1) + ":" + currentHash
        val eventId = "boox-finalize-" + sha256Hex(eventIdentity.toByteArray())
        val descriptor = File(outgoingDir(documentRoot), outputName + ".inkbridge.json")
        val descriptorJson = JSONObject()
            .put("schemaVersion", 1)
            .put("eventId", eventId)
            .put("documentId", state.documentId)
            .put("source", "boox")
            .put("objectPath", "BOOX_Folder/" + state.documentId + "/" + outputName)
            .put("sourceGeneration", localGeneration)
            .put("sourceRevision", state.activeRevisions.boox + 1)
            .put("basedOn", state.activeRevisions.toJson())
            .put("contentSha256", currentHash)
            .put("payloadKind", "device_view")
        publishBytesOrVerify(descriptorJson.toString(2).toByteArray(), descriptor)
        syncDirectory(outgoingDir(documentRoot))
        return output to descriptor
    }
    private fun installAccepted(
        delivery: BrokerDelivery,
        incomingPdf: File,
        documentRoot: File,
        previous: HandoffState?,
        previousActive: File?,
        previousActiveHash: String?,
    ): InstallResult.Installed {
        val active = activeDir(documentRoot)
        val stem = delivery.originalFileName.substringBeforeLast('.').sanitizeStem()
        val newName = "${stem}__ib-b${delivery.sourceRevisions.boox}" +
            "-s${delivery.sourceRevisions.supernote}-g${delivery.sourceGeneration}.pdf"
        val destination = File(active, newName)
        val processed = ((previous?.processedEventIds ?: emptyList()) + delivery.eventId)
            .distinct()
            .takeLast(512)
        val next = HandoffState(
            documentId = delivery.documentId,
            originalFileName = delivery.originalFileName,
            activeRevisions = delivery.sourceRevisions,
            sourceGeneration = delivery.sourceGeneration,
            brokerEventId = delivery.eventId,
            activeFileName = newName,
            installedBrokerSha256 = delivery.contentSha256,
            processedEventIds = processed,
        )
        next.validate()
        val intent = InstallIntent(
            previousActiveFileName = previousActive?.name,
            previousActiveSha256 = previousActiveHash,
            nextState = next,
        )
        writeInstallIntent(documentRoot, intent)

        // Keep the predecessor active until both the replacement and its durable
        // state are committed. recoverInstall() completes or safely restarts each
        // intermediate state after process death or power loss.
        publishFileOrVerify(incomingPdf, destination, delivery.contentSha256)
        beforeInstallCommitForTest?.invoke(previousActive)
        if (preserveChangedPredecessor(documentRoot, previous, intent, destination)) {
            error("The active PDF changed during installation; its edits were preserved. Retry the update")
        }
        writeState(documentRoot, next)
        retirePreviousActive(documentRoot, intent)
        clearInstallIntent(documentRoot)
        return InstallResult.Installed(destination, next)
    }
    private fun documentRoot(documentId: String): File {
        requireDocumentId(documentId)
        return File(root, documentId)
    }

    private fun activeDir(documentRoot: File) = File(documentRoot, "active").also(File::mkdirs)
    private fun incomingDir(documentRoot: File) = File(documentRoot, "incoming")
    private fun retiredDir(documentRoot: File) = File(documentRoot, ".retired").also(File::mkdirs)
    private fun outgoingDir(documentRoot: File) = File(documentRoot, "outgoing").also(File::mkdirs)
    private fun stateFile(documentRoot: File) = File(documentRoot, ".inkbridge-state.json")
    private fun installIntentFile(documentRoot: File) = File(documentRoot, ".inkbridge-install.json")
    private fun finalizeIntentFile(documentRoot: File) = File(documentRoot, ".inkbridge-finalize.json")

    private fun readState(documentRoot: File): HandoffState? = stateFile(documentRoot)
        .takeIf(File::isFile)
        ?.let { parseState(it, documentRoot) }

    private fun parseState(file: File, documentRoot: File): HandoffState =
        HandoffState.fromJson(JSONObject(file.readText())).also { state ->
            require(state.documentId == documentRoot.name) { "Handoff state belongs to a different document" }
        }

    private fun writeState(documentRoot: File, state: HandoffState) {
        documentRoot.mkdirs()
        val current = stateFile(documentRoot)
        val next = File(documentRoot, ".inkbridge-state.next")
        val previous = File(documentRoot, ".inkbridge-state.previous")
        writeSynced(next, state.toJson().toString(2).toByteArray())
        if (current.isFile) {
            deleteAndSync(previous, documentRoot)
            moveNoReplace(current, previous)
            syncDirectory(documentRoot)
        }
        try {
            moveNoReplace(next, current)
            syncDirectory(documentRoot)
            deleteAndSync(previous, documentRoot)
        } catch (error: Throwable) {
            if (!current.exists() && previous.isFile) {
                moveNoReplace(previous, current)
                syncDirectory(documentRoot)
            }
            throw error
        }
    }

    private fun recoverState(documentRoot: File) {
        val current = stateFile(documentRoot)
        val next = File(documentRoot, ".inkbridge-state.next")
        val previous = File(documentRoot, ".inkbridge-state.previous")
        val currentValid = current.takeIf(File::isFile)?.let {
            runCatching { parseState(it, documentRoot) }.getOrNull()
        }
        if (currentValid != null) {
            deleteAndSync(next, documentRoot)
            deleteAndSync(previous, documentRoot)
            return
        }

        val nextValid = next.takeIf(File::isFile)?.let {
            runCatching { parseState(it, documentRoot) }.getOrNull()
        }
        val previousValid = previous.takeIf(File::isFile)?.let {
            runCatching { parseState(it, documentRoot) }.getOrNull()
        }
        when {
            current.isFile && nextValid != null -> {
                preserveCorruptState(current, documentRoot)
                moveNoReplace(next, current)
                deleteAndSync(previous, documentRoot)
            }
            current.isFile && previousValid != null -> {
                preserveCorruptState(current, documentRoot)
                if (next.isFile) preserveCorruptState(next, documentRoot)
                moveNoReplace(previous, current)
            }
            !current.exists() && nextValid != null -> {
                moveNoReplace(next, current)
                deleteAndSync(previous, documentRoot)
            }
            !current.exists() && previousValid != null -> {
                if (next.isFile) preserveCorruptState(next, documentRoot)
                moveNoReplace(previous, current)
            }
            current.isFile -> parseState(current, documentRoot)
            current.exists() -> error("Handoff state path is not a file")
            next.isFile -> parseState(next, documentRoot)
            previous.isFile -> parseState(previous, documentRoot)
        }
    }

    private fun preserveCorruptState(file: File, documentRoot: File) {
        val contentHash = sha256Hex(file)
        val preserved = File(documentRoot, file.name + ".corrupt-" + contentHash)
        if (preserved.isFile) {
            require(sha256Hex(preserved) == contentHash) { "Conflicting preserved corrupt state" }
            deleteAndSync(file, documentRoot)
        } else {
            moveNoReplace(file, preserved)
        }
    }

    private fun recover(documentRoot: File) {
        recoverState(documentRoot)
        recoverFinalize(documentRoot)
        recoverMissingActive(documentRoot)
        recoverInstall(documentRoot)
        recoverMissingActive(documentRoot)
        requireStateForActiveFiles(documentRoot)
    }

    private fun requireStateForActiveFiles(documentRoot: File) {
        if (readState(documentRoot) != null) return
        val activeFiles = File(documentRoot, "active").listFiles().orEmpty()
        require(activeFiles.isEmpty()) {
            "Handoff state is missing while active PDF files remain; refusing to install or open a revision"
        }
    }

    private fun recoverMissingActive(documentRoot: File) {
        val state = readState(documentRoot) ?: return
        val active = File(activeDir(documentRoot), state.activeFileName)
        if (active.exists()) {
            require(active.isFile) { "The active PDF path is not a file" }
            return
        }

        val expectedHash: String
        val recoverySource: File?
        if (state.finalizedLocalSha256 != null) {
            expectedHash = state.finalizedLocalSha256
            recoverySource = state.finalizedOutputFileName
                ?.let { File(outgoingDir(documentRoot), it) }
                ?.takeIf(File::isFile)
        } else {
            expectedHash = state.installedBrokerSha256
            recoverySource = findRetainedBrokerPdf(documentRoot, state)
        }
        if (recoverySource == null) return
        cleanupStagedPublications(active.parentFile!!, active.name, "active PDF recovery")
        publishFileOrVerify(recoverySource, active, expectedHash)
    }

    private fun findRetainedBrokerPdf(documentRoot: File, state: HandoffState): File? =
        incomingDir(documentRoot)
            .listFiles()
            .orEmpty()
            .asSequence()
            .filter { it.isFile && it.name.endsWith(".inkbridge.json") }
            .mapNotNull { descriptor ->
                runCatching {
                    val delivery = BrokerDelivery.fromJson(JSONObject(descriptor.readText()))
                    if (
                        delivery.eventId != state.brokerEventId ||
                        delivery.documentId != state.documentId ||
                        delivery.sourceRevisions != state.activeRevisions ||
                        delivery.sourceGeneration != state.sourceGeneration ||
                        delivery.contentSha256 != state.installedBrokerSha256
                    ) {
                        return@runCatching null
                    }
                    File(descriptor.parentFile, delivery.pdfFileName).takeIf(File::isFile)
                }.getOrNull()
            }
            .firstOrNull()

    private fun writeFinalizeIntent(documentRoot: File, intent: FinalizeIntent) {
        intent.validate(documentRoot.name)
        publishBytesOrVerify(
            intent.toJson().toString(2).toByteArray(),
            finalizeIntentFile(documentRoot),
        )
    }

    private fun recoverFinalize(documentRoot: File) {
        val intentFile = finalizeIntentFile(documentRoot)
        if (!intentFile.isFile) return
        val intent = FinalizeIntent.fromJson(JSONObject(intentFile.readText()), documentRoot.name)
        val previous = intent.previousState
        val next = intent.nextState
        val current = readState(documentRoot) ?: error("Finalize intent has no handoff state")
        require(current == previous || current == next) {
            "Finalize intent does not match the committed handoff state"
        }

        val expectedHash = requireNotNull(next.finalizedLocalSha256)
        val outputName = requireNotNull(next.finalizedOutputFileName)
        val outgoing = outgoingDir(documentRoot)
        cleanupStagedPublications(outgoing, outputName, "finalized PDF")
        cleanupStagedPublications(outgoing, "$outputName.inkbridge.json", "finalized descriptor")
        val output = File(outgoing, outputName)
        val active = File(activeDir(documentRoot), previous.activeFileName)
        val activeHash = active.takeIf(File::isFile)?.let(::sha256Hex)
        if (current == previous && !output.isFile && activeHash != expectedHash) {
            clearFinalizeIntent(documentRoot)
            return
        }

        ensureFinalizedArtifacts(
            documentRoot,
            previous,
            active,
            expectedHash,
            outputName,
            next.localGeneration,
        )
        if (current == previous) writeState(documentRoot, next)
        clearFinalizeIntent(documentRoot)
    }

    private fun writeInstallIntent(documentRoot: File, intent: InstallIntent) {
        intent.validate(documentRoot.name)
        publishBytesOrVerify(
            intent.toJson().toString(2).toByteArray(),
            installIntentFile(documentRoot),
        )
    }

    private fun recoverInstall(documentRoot: File) {
        val intentFile = installIntentFile(documentRoot)
        if (!intentFile.isFile) return
        val intent = InstallIntent.fromJson(JSONObject(intentFile.readText()), documentRoot.name)
        val next = intent.nextState
        val current = readState(documentRoot)
        val nextActive = File(activeDir(documentRoot), next.activeFileName)
        cleanupStagedPublications(activeDir(documentRoot), next.activeFileName, "active PDF")

        if (!nextActive.exists()) {
            require(current?.brokerEventId != next.brokerEventId) {
                "Committed handoff state points to a missing active PDF"
            }
            clearInstallIntent(documentRoot)
            return
        }
        require(nextActive.isFile) { "Replacement active path is not a PDF file" }
        require(sha256Hex(nextActive) == next.installedBrokerSha256) {
            "Replacement active PDF has unexpected content"
        }

        when {
            current == next -> Unit
            current == null -> writeState(documentRoot, next)
            current.brokerEventId == next.brokerEventId -> {
                error("Install intent does not match the committed handoff state")
            }
            else -> {
                require(intent.previousActiveFileName == current.activeFileName) {
                    "Install intent predecessor does not match the active handoff state"
                }
                require(next.activeRevisions.strictlyDominates(current.activeRevisions)) {
                    "Install intent is stale or conflicts with the active revisions"
                }
                require(next.sourceGeneration > current.sourceGeneration) {
                    "Install intent generation is not newer"
                }
                if (preserveChangedPredecessor(documentRoot, current, intent, nextActive)) return
                writeState(documentRoot, next)
            }
        }

        retirePreviousActive(documentRoot, intent)
        clearInstallIntent(documentRoot)
    }

    private fun preserveChangedPredecessor(
        documentRoot: File,
        current: HandoffState?,
        intent: InstallIntent,
        replacement: File,
    ): Boolean {
        val previousName = intent.previousActiveFileName ?: return false
        val expectedHash = requireNotNull(intent.previousActiveSha256)
        val previousState = requireNotNull(current) { "Install intent predecessor state is missing" }
        require(previousState.activeFileName == previousName) {
            "Install intent predecessor does not match the active handoff state"
        }
        val previous = File(activeDir(documentRoot), previousName)
        val commitHash = previous.takeIf(File::isFile)?.let(::sha256Hex)
        if (commitHash == expectedHash) return false

        if (replacement.exists()) {
            require(replacement.isFile) { "Replacement active path is not a PDF file" }
            require(sha256Hex(replacement) == intent.nextState.installedBrokerSha256) {
                "Replacement active PDF changed before install cancellation"
            }
            deleteAndSync(replacement, activeDir(documentRoot))
        }
        clearInstallIntent(documentRoot)
        if (
            commitHash != null &&
            commitHash != previousState.installedBrokerSha256 &&
            commitHash != previousState.finalizedLocalSha256
        ) {
            commitFinalization(documentRoot, previousState, previous, commitHash)
        }
        return true
    }

    private fun cleanupStagedPublications(directory: File, destinationName: String, description: String) {
        val prefix = ".$destinationName."
        var deleted = false
        directory.listFiles().orEmpty()
            .filter { it.isFile && it.name.startsWith(prefix) && it.name.endsWith(".tmp") }
            .forEach { staged ->
                require(staged.delete()) { "Could not remove interrupted $description copy " + staged.name }
                deleted = true
            }
        if (deleted) syncDirectory(directory)
    }

    private fun retirePreviousActive(documentRoot: File, intent: InstallIntent) {
        val previousName = intent.previousActiveFileName ?: return
        if (previousName == intent.nextState.activeFileName) return
        val previous = File(activeDir(documentRoot), previousName)
        if (!previous.exists()) return
        require(previous.isFile) { "Previous active path is not a PDF file" }
        val retired = File(retiredDir(documentRoot), previousName)
        require(!retired.exists()) { "Refusing to overwrite retired " + retired.name }
        moveNoReplace(previous, retired)
    }

    private fun clearFinalizeIntent(documentRoot: File) {
        deleteAndSync(finalizeIntentFile(documentRoot), documentRoot)
    }

    private fun clearInstallIntent(documentRoot: File) {
        deleteAndSync(installIntentFile(documentRoot), documentRoot)
    }

    internal fun publishFileOrVerify(source: File, destination: File, expectedHash: String) {
        if (destination.isFile) {
            require(sha256Hex(destination) == expectedHash) {
                "Existing ${destination.name} has unexpected content"
            }
            return
        }
        publishCreateOnly(source, destination, expectedHash)
    }

    private fun publishBytesOrVerify(bytes: ByteArray, destination: File) {
        if (destination.isFile) {
            require(destination.readBytes().contentEquals(bytes)) {
                "Existing ${destination.name} has unexpected content"
            }
            return
        }
        publishBytesCreateOnly(bytes, destination)
    }

    private fun publishCreateOnly(source: File, destination: File, expectedHash: String) {
        destination.parentFile?.mkdirs()
        require(!destination.exists()) { "Refusing to overwrite " + destination.name }
        val temp = File(
            destination.parentFile,
            "." + destination.name + "." + System.nanoTime() + ".tmp",
        )
        try {
            val digest = MessageDigest.getInstance("SHA-256")
            FileOutputStream(temp).use { output ->
                source.inputStream().buffered().use { input ->
                    val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                    while (true) {
                        val count = input.read(buffer)
                        if (count < 0) break
                        if (count > 0) {
                            output.write(buffer, 0, count)
                            digest.update(buffer, 0, count)
                        }
                    }
                }
                output.fd.sync()
            }
            val copiedHash = digest.digest().joinToString("") { "%02x".format(it) }
            require(copiedHash == expectedHash) {
                "Source " + source.name + " changed while it was being copied"
            }
            publishTempCreateOnly(temp, destination)
        } finally {
            temp.delete()
        }
    }
    private fun publishBytesCreateOnly(bytes: ByteArray, destination: File) {
        destination.parentFile?.mkdirs()
        require(!destination.exists()) { "Refusing to overwrite ${destination.name}" }
        val temp = File(destination.parentFile, ".${destination.name}.${System.nanoTime()}.tmp")
        writeSynced(temp, bytes)
        try {
            publishTempCreateOnly(temp, destination)
        } finally {
            temp.delete()
        }
    }

    private fun publishTempCreateOnly(temp: File, destination: File) {
        val directory = requireNotNull(destination.parentFile)
        try {
            Files.createLink(destination.toPath(), temp.toPath())
            require(temp.delete()) { "Could not remove staged copy " + temp.name }
            syncDirectory(directory)
        } catch (error: Exception) {
            if (destination.exists()) throw error
            moveNoReplace(temp, destination)
            syncDirectory(directory)
        }
    }

    private fun deleteAndSync(file: File, directory: File) {
        if (!file.exists()) return
        require(file.delete()) { "Could not remove " + file.name }
        syncDirectory(directory)
    }

    private fun syncDirectory(directory: File) {
        try {
            FileChannel.open(directory.toPath(), StandardOpenOption.READ).use { it.force(true) }
        } catch (error: AccessDeniedException) {
            if (!(System.getProperty("os.name") ?: "").startsWith("Windows", ignoreCase = true)) throw error
        }
    }

    private fun writeSynced(file: File, bytes: ByteArray) {
        file.parentFile?.mkdirs()
        FileOutputStream(file).use {
            it.write(bytes)
            it.fd.sync()
        }
    }

    private fun moveNoReplace(source: File, destination: File) {
        val sourceDirectory = requireNotNull(source.parentFile)
        val destinationDirectory = requireNotNull(destination.parentFile).also(File::mkdirs)
        // ATOMIC_MOVE may replace a target that appears concurrently. With no
        // REPLACE_EXISTING option, the provider must fail rather than overwrite.
        Files.move(source.toPath(), destination.toPath())
        syncDirectory(destinationDirectory)
        if (sourceDirectory.absolutePath != destinationDirectory.absolutePath) {
            syncDirectory(sourceDirectory)
        }
    }
}

private fun String.sanitizeStem(): String = replace(Regex("[^A-Za-z0-9._ -]"), "_")
    .trim()
    .take(100)
    .ifBlank { "document" }

private fun requireDocumentId(value: String) {
    require(Regex("inkbridge-doc-v1-[0-9a-f]{64}").matches(value)) { "Invalid document ID" }
}

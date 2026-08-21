package dev.inkbridge.boox

import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.Files
import java.nio.file.StandardCopyOption
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

class BooxHandoffStore(val root: File) {
    fun install(descriptorFile: File): InstallResult {
        val delivery = BrokerDelivery.fromJson(JSONObject(descriptorFile.readText()))
        val documentRoot = documentRoot(delivery.documentId)
        val incomingPdf = File(descriptorFile.parentFile, delivery.pdfFileName)
        require(incomingPdf.isFile) { "Incoming PDF is missing" }
        val incomingHash = sha256Hex(incomingPdf)
        require(incomingHash == delivery.contentSha256) { "Incoming PDF hash does not match its descriptor" }

        recover(documentRoot)
        val previous = readState(documentRoot)
        val previousActive = previous?.let { File(activeDir(documentRoot), it.activeFileName) }
        val activeHash = previousActive?.takeIf(File::isFile)?.let(::sha256Hex)
        return when (val decision = HandoffPolicy.decideInstall(previous, delivery, activeHash)) {
            InstallDecision.Duplicate -> InstallResult.Duplicate(previousActive)
            is InstallDecision.Reject -> error(decision.reason)
            InstallDecision.Install -> installAccepted(
                delivery,
                incomingPdf,
                documentRoot,
                previous,
                previousActive,
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
        if (currentHash == state.installedBrokerSha256) return FinalizeResult.NoChanges
        if (currentHash == state.finalizedLocalSha256) {
            val outputName = requireNotNull(state.finalizedOutputFileName)
            val artifacts = ensureFinalizedArtifacts(
                documentRoot,
                state,
                active,
                currentHash,
                outputName,
                state.localGeneration,
            )
            return FinalizeResult.AlreadyFinalized(artifacts.first)
        }

        val nextLocalGeneration = state.localGeneration + 1
        val outputName = active.nameWithoutExtension +
            "__boox-finalized-g" + nextLocalGeneration + "-" + currentHash.take(12) + ".pdf"
        val artifacts = ensureFinalizedArtifacts(
            documentRoot,
            state,
            active,
            currentHash,
            outputName,
            nextLocalGeneration,
        )
        val next = state.copy(
            finalizedLocalSha256 = currentHash,
            finalizedOutputFileName = outputName,
            localGeneration = nextLocalGeneration,
        )
        writeState(documentRoot, next)
        return FinalizeResult.Finalized(artifacts.first, artifacts.second, next)
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
                val incomingPdf = File(descriptor.parentFile, delivery.pdfFileName)
                require(incomingPdf.isFile) { "Incoming PDF is missing" }
                require(sha256Hex(incomingPdf) == delivery.contentSha256) {
                    "Incoming PDF hash does not match its descriptor"
                }
                val deliveryRoot = documentRoot(delivery.documentId)
                recover(deliveryRoot)
                val state = readState(deliveryRoot)
                when {
                    state == null -> true
                    delivery.eventId in state.processedEventIds -> false
                    delivery.sourceRevisions == state.activeRevisions -> false
                    !delivery.sourceRevisions.strictlyDominates(state.activeRevisions) -> false
                    delivery.sourceGeneration <= state.sourceGeneration -> false
                    state.finalizedLocalSha256 != null &&
                        delivery.sourceRevisions.boox <= state.activeRevisions.boox -> false
                    else -> true
                }
            }.getOrDefault(false)
        }

    fun findMostRecentState(): HandoffState? = root
        .listFiles()
        .orEmpty()
        .asSequence()
        .filter { it.isDirectory && it.name.startsWith("inkbridge-doc-v1-") }
        .mapNotNull { documentRoot ->
            recover(documentRoot)
            readState(documentRoot)
        }
        .maxByOrNull { it.sourceGeneration }

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
        val eventId = "boox-finalize-" +
            sha256Hex((state.documentId + ":" + currentHash).toByteArray())
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
        return output to descriptor
    }
    private fun installAccepted(
        delivery: BrokerDelivery,
        incomingPdf: File,
        documentRoot: File,
        previous: HandoffState?,
        previousActive: File?,
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
            nextState = next,
        )
        writeInstallIntent(documentRoot, intent)

        // Keep the predecessor active until both the replacement and its durable
        // state are committed. recoverInstall() completes or safely restarts each
        // intermediate state after process death or power loss.
        publishFileOrVerify(incomingPdf, destination, delivery.contentSha256)
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

    private fun readState(documentRoot: File): HandoffState? = stateFile(documentRoot)
        .takeIf(File::isFile)
        ?.let { HandoffState.fromJson(JSONObject(it.readText())) }

    private fun writeState(documentRoot: File, state: HandoffState) {
        documentRoot.mkdirs()
        val current = stateFile(documentRoot)
        val next = File(documentRoot, ".inkbridge-state.next")
        val previous = File(documentRoot, ".inkbridge-state.previous")
        writeSynced(next, state.toJson().toString(2).toByteArray())
        if (current.isFile) {
            previous.delete()
            moveNoReplace(current, previous)
        }
        try {
            moveNoReplace(next, current)
            previous.delete()
        } catch (error: Throwable) {
            if (!current.exists() && previous.isFile) moveNoReplace(previous, current)
            throw error
        }
    }

    private fun recoverState(documentRoot: File) {
        val current = stateFile(documentRoot)
        val next = File(documentRoot, ".inkbridge-state.next")
        val previous = File(documentRoot, ".inkbridge-state.previous")
        when {
            current.isFile -> {
                next.delete()
                previous.delete()
            }
            next.isFile -> {
                moveNoReplace(next, current)
                previous.delete()
            }
            previous.isFile -> moveNoReplace(previous, current)
        }
    }

    private fun recover(documentRoot: File) {
        recoverState(documentRoot)
        recoverMissingActive(documentRoot)
        recoverInstall(documentRoot)
        recoverMissingActive(documentRoot)
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
                writeState(documentRoot, next)
            }
        }

        retirePreviousActive(documentRoot, intent)
        clearInstallIntent(documentRoot)
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

    private fun clearInstallIntent(documentRoot: File) {
        val intent = installIntentFile(documentRoot)
        if (intent.exists()) {
            require(intent.delete()) { "Could not clear completed install intent" }
        }
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
        try {
            Files.createLink(destination.toPath(), temp.toPath())
            temp.delete()
        } catch (error: Exception) {
            if (destination.exists()) throw error
            moveNoReplace(temp, destination)
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
        destination.parentFile?.mkdirs()
        require(!destination.exists()) { "Refusing to overwrite ${destination.name}" }
        try {
            Files.move(source.toPath(), destination.toPath(), StandardCopyOption.ATOMIC_MOVE)
        } catch (_: AtomicMoveNotSupportedException) {
            Files.move(source.toPath(), destination.toPath())
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

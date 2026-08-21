package dev.inkbridge.boox

import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.Files
import java.nio.file.StandardCopyOption

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

        recoverState(documentRoot)
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
        recoverState(documentRoot)
        val state = readState(documentRoot) ?: error("No active InkBridge document")
        val active = File(activeDir(documentRoot), state.activeFileName)
        require(active.isFile) { "The active PDF is missing" }
        val currentHash = sha256Hex(active)
        if (currentHash == state.installedBrokerSha256) return FinalizeResult.NoChanges
        if (currentHash == state.finalizedLocalSha256) {
            return FinalizeResult.AlreadyFinalized(
                state.finalizedOutputFileName?.let { File(outgoingDir(documentRoot), it) },
            )
        }

        val nextLocalGeneration = state.localGeneration + 1
        val outputName = active.nameWithoutExtension +
            "__boox-finalized-g${nextLocalGeneration}-${currentHash.take(12)}.pdf"
        val output = File(outgoingDir(documentRoot), outputName)
        publishFileOrVerify(active, output, currentHash)
        val eventId = "boox-finalize-${sha256Hex("${state.documentId}:$currentHash".toByteArray())}"
        val descriptor = File(outgoingDir(documentRoot), "$outputName.inkbridge.json")
        val descriptorJson = JSONObject()
            .put("schemaVersion", 1)
            .put("eventId", eventId)
            .put("documentId", state.documentId)
            .put("source", "boox")
            .put("objectPath", "BOOX_Folder/${state.documentId}/$outputName")
            .put("sourceGeneration", nextLocalGeneration)
            .put("sourceRevision", state.activeRevisions.boox + 1)
            .put("basedOn", state.activeRevisions.toJson())
            .put("contentSha256", currentHash)
            .put("payloadKind", "device_view")
        val descriptorBytes = descriptorJson.toString(2).toByteArray()
        publishBytesOrVerify(descriptorBytes, descriptor)

        val next = state.copy(
            finalizedLocalSha256 = currentHash,
            finalizedOutputFileName = outputName,
            localGeneration = nextLocalGeneration,
        )
        writeState(documentRoot, next)
        return FinalizeResult.Finalized(output, descriptor, next)
    }

    fun state(documentId: String): HandoffState? {
        requireDocumentId(documentId)
        val documentRoot = documentRoot(documentId)
        recoverState(documentRoot)
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
                val state = readState(documentRoot(delivery.documentId))
                state == null || delivery.eventId !in state.processedEventIds
            }.getOrDefault(true)
        }

    fun findMostRecentState(): HandoffState? = root
        .listFiles()
        .orEmpty()
        .asSequence()
        .filter { it.isDirectory && it.name.startsWith("inkbridge-doc-v1-") }
        .mapNotNull { readState(it) }
        .maxByOrNull { it.sourceGeneration }

    private fun installAccepted(
        delivery: BrokerDelivery,
        incomingPdf: File,
        documentRoot: File,
        previous: HandoffState?,
        previousActive: File?,
    ): InstallResult.Installed {
        val active = activeDir(documentRoot)
        val retired = retiredDir(documentRoot)
        val stem = delivery.originalFileName.substringBeforeLast('.').sanitizeStem()
        val newName = "${stem}__ib-b${delivery.sourceRevisions.boox}" +
            "-s${delivery.sourceRevisions.supernote}-g${delivery.sourceGeneration}.pdf"
        val destination = File(active, newName)
        require(!destination.exists()) { "Revisioned destination already exists unexpectedly" }

        var retiredPrevious: File? = null
        try {
            if (previousActive?.isFile == true) {
                retiredPrevious = File(retired, previousActive.name)
                moveNoReplace(previousActive, retiredPrevious)
            }
            publishCreateOnly(incomingPdf, destination)
        } catch (error: Throwable) {
            destination.delete()
            if (retiredPrevious?.isFile == true && previousActive != null && !previousActive.exists()) {
                moveNoReplace(retiredPrevious, previousActive)
            }
            throw error
        }

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
        try {
            writeState(documentRoot, next)
        } catch (error: Throwable) {
            destination.delete()
            if (retiredPrevious?.isFile == true && previousActive != null && !previousActive.exists()) {
                moveNoReplace(retiredPrevious, previousActive)
            }
            throw error
        }
        return InstallResult.Installed(destination, next)
    }

    private fun documentRoot(documentId: String): File {
        requireDocumentId(documentId)
        return File(root, documentId)
    }

    private fun activeDir(documentRoot: File) = File(documentRoot, "active").also(File::mkdirs)
    private fun retiredDir(documentRoot: File) = File(documentRoot, ".retired").also(File::mkdirs)
    private fun outgoingDir(documentRoot: File) = File(documentRoot, "outgoing").also(File::mkdirs)
    private fun stateFile(documentRoot: File) = File(documentRoot, ".inkbridge-state.json")

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

    private fun publishFileOrVerify(source: File, destination: File, expectedHash: String) {
        if (destination.isFile) {
            require(sha256Hex(destination) == expectedHash) {
                "Existing ${destination.name} has unexpected content"
            }
            return
        }
        publishCreateOnly(source, destination)
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

    private fun publishCreateOnly(source: File, destination: File) {
        destination.parentFile?.mkdirs()
        require(!destination.exists()) { "Refusing to overwrite ${destination.name}" }
        val temp = File(destination.parentFile, ".${destination.name}.${System.nanoTime()}.tmp")
        FileOutputStream(temp).use { output ->
            source.inputStream().buffered().use { input -> input.copyTo(output) }
            output.fd.sync()
        }
        try {
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

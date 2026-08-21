package dev.inkbridge.boox

import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.security.MessageDigest

internal const val BROKER_PRODUCER = "inkbridge-broker"
internal const val DESCRIPTOR_SCHEMA_VERSION = 1
internal const val MAX_DESCRIPTOR_BYTES = 256 * 1024L
internal const val SAFE_FILE_NAME_MAX_BYTES = 180
private val DOCUMENT_ID = Regex("inkbridge-doc-v1-[0-9a-f]{64}")
private val SHA256 = Regex("[0-9a-f]{64}")

data class RevisionPair(val boox: Long, val supernote: Long) {
    init {
        require(boox >= 0 && supernote >= 0) { "Revisions cannot be negative" }
    }

    fun dominates(other: RevisionPair): Boolean =
        boox >= other.boox && supernote >= other.supernote

    fun strictlyDominates(other: RevisionPair): Boolean = dominates(other) && this != other

    fun toJson(): JSONObject = JSONObject().put("boox", boox).put("supernote", supernote)

    companion object {
        fun fromJson(value: JSONObject) = RevisionPair(
            value.getLong("boox"),
            value.getLong("supernote"),
        )
    }
}

data class BrokerDelivery(
    val schemaVersion: Int,
    val producer: String,
    val eventId: String,
    val documentId: String,
    val originalFileName: String,
    val sourceRevisions: RevisionPair,
    val sourceGeneration: Long,
    val contentSha256: String,
    val pdfFileName: String,
) {
    fun validate() {
        require(schemaVersion == DESCRIPTOR_SCHEMA_VERSION) { "Unsupported descriptor version" }
        require(producer == BROKER_PRODUCER) { "Not an InkBridge broker output" }
        require(eventId.isNotBlank() && eventId.length <= 256) { "Invalid event ID" }
        require(DOCUMENT_ID.matches(documentId)) { "Invalid stable document ID" }
        require(sourceGeneration >= 1) { "Invalid source generation" }
        require(SHA256.matches(contentSha256)) { "Invalid content hash" }
        requireSafeFileName(originalFileName, "original file name")
        requireSafeFileName(pdfFileName, "PDF file name")
        require(pdfFileName.endsWith(".pdf", ignoreCase = true)) { "Delivery is not a PDF" }
    }

    fun toJson(): JSONObject = JSONObject()
        .put("schemaVersion", schemaVersion)
        .put("producer", producer)
        .put("eventId", eventId)
        .put("documentId", documentId)
        .put("originalFileName", originalFileName)
        .put("sourceRevisions", sourceRevisions.toJson())
        .put("sourceGeneration", sourceGeneration)
        .put("contentSha256", contentSha256)
        .put("pdfFileName", pdfFileName)

    companion object {
        fun fromJson(value: JSONObject) = BrokerDelivery(
            schemaVersion = value.getInt("schemaVersion"),
            producer = value.getString("producer"),
            eventId = value.getString("eventId"),
            documentId = value.getString("documentId"),
            originalFileName = value.getString("originalFileName"),
            sourceRevisions = RevisionPair.fromJson(value.getJSONObject("sourceRevisions")),
            sourceGeneration = value.getLong("sourceGeneration"),
            contentSha256 = value.getString("contentSha256"),
            pdfFileName = value.getString("pdfFileName"),
        ).also { it.validate() }
    }
}

data class HandoffState(
    val schemaVersion: Int = 1,
    val documentId: String,
    val originalFileName: String,
    val activeRevisions: RevisionPair,
    val sourceGeneration: Long,
    val brokerEventId: String,
    val activeFileName: String,
    val installedBrokerSha256: String,
    val finalizedLocalSha256: String? = null,
    val finalizedOutputFileName: String? = null,
    val localGeneration: Long = 0,
    val processedEventIds: List<String> = emptyList(),
    val retiredPredecessors: List<RetiredPredecessorWatch> = emptyList(),
) {
    fun validate() {
        require(schemaVersion == 1) { "Unsupported handoff state version" }
        require(DOCUMENT_ID.matches(documentId)) { "Invalid stable document ID" }
        requireSafeFileName(originalFileName, "original file name")
        requireSafeFileName(activeFileName, "active file name")
        require(sourceGeneration >= 1) { "Invalid source generation" }
        require(brokerEventId.isNotBlank() && brokerEventId.length <= 256) { "Invalid broker event ID" }
        require(SHA256.matches(installedBrokerSha256)) { "Invalid installed PDF hash" }
        finalizedLocalSha256?.let { require(SHA256.matches(it)) { "Invalid finalized PDF hash" } }
        finalizedOutputFileName?.let { requireSafeFileName(it, "finalized output file name") }
        require((finalizedLocalSha256 == null) == (finalizedOutputFileName == null)) {
            "Finalized PDF hash and file name must be recorded together"
        }
        require(localGeneration >= 0) { "Invalid local generation" }
        require(processedEventIds.all { it.isNotBlank() && it.length <= 256 }) {
            "Invalid processed event ID"
        }
        retiredPredecessors.forEach { it.validate(documentId) }
        require(retiredPredecessors.map { it.retiredFileName }.distinct().size == retiredPredecessors.size) {
            "Duplicate retired predecessor watch"
        }
    }
    fun toJson(): JSONObject = JSONObject()
        .put("schemaVersion", schemaVersion)
        .put("documentId", documentId)
        .put("originalFileName", originalFileName)
        .put("activeRevisions", activeRevisions.toJson())
        .put("sourceGeneration", sourceGeneration)
        .put("brokerEventId", brokerEventId)
        .put("activeFileName", activeFileName)
        .put("installedBrokerSha256", installedBrokerSha256)
        .putOpt("finalizedLocalSha256", finalizedLocalSha256)
        .putOpt("finalizedOutputFileName", finalizedOutputFileName)
        .put("localGeneration", localGeneration)
        .put("processedEventIds", JSONArray(processedEventIds))
        .put("retiredPredecessors", JSONArray(retiredPredecessors.map { it.toJson() }))

    companion object {
        fun fromJson(value: JSONObject): HandoffState {
            val eventIds = value.optJSONArray("processedEventIds") ?: JSONArray()
            val documentId = value.getString("documentId")
            val retiredPredecessors = value.optJSONArray("retiredPredecessors") ?: JSONArray()
            return HandoffState(
                schemaVersion = value.getInt("schemaVersion"),
                documentId = documentId,
                originalFileName = value.getString("originalFileName"),
                activeRevisions = RevisionPair.fromJson(value.getJSONObject("activeRevisions")),
                sourceGeneration = value.getLong("sourceGeneration"),
                brokerEventId = value.getString("brokerEventId"),
                activeFileName = value.getString("activeFileName"),
                installedBrokerSha256 = value.getString("installedBrokerSha256"),
                finalizedLocalSha256 = value.optNullableString("finalizedLocalSha256"),
                finalizedOutputFileName = value.optNullableString("finalizedOutputFileName"),
                localGeneration = value.optLong("localGeneration", 0),
                processedEventIds = List(eventIds.length()) { eventIds.getString(it) },
                retiredPredecessors = List(retiredPredecessors.length()) { index ->
                    RetiredPredecessorWatch.fromJson(retiredPredecessors.getJSONObject(index), documentId)
                },
            ).also { it.validate() }
        }
    }
}


data class InstallIntent(
    val schemaVersion: Int = 3,
    val previousState: HandoffState?,
    val previousActiveSha256: String?,
    val nextState: HandoffState,
) {
    fun validate(documentId: String) {
        require(schemaVersion == 3) { "Unsupported install intent version" }
        previousState?.validate()
        nextState.validate()
        require(nextState.documentId == documentId) { "Install intent belongs to a different document" }
        previousActiveSha256?.let { require(SHA256.matches(it)) { "Invalid previous active PDF hash" } }
        require((previousState == null) == (previousActiveSha256 == null)) {
            "Install intent predecessor state and hash must be recorded together"
        }
        previousState?.let { previous ->
            require(previous.documentId == documentId) { "Install intent predecessor belongs elsewhere" }
            require(nextState.activeRevisions.strictlyDominates(previous.activeRevisions)) {
                "Install intent revisions do not advance the predecessor"
            }
            require(nextState.sourceGeneration > previous.sourceGeneration) {
                "Install intent generation is not newer than the predecessor"
            }
        }
    }

    fun toJson(): JSONObject = JSONObject()
        .put("schemaVersion", schemaVersion)
        .putOpt("previousState", previousState?.toJson())
        .putOpt("previousActiveSha256", previousActiveSha256)
        .put("nextState", nextState.toJson())

    companion object {
        fun fromJson(value: JSONObject, documentId: String) = InstallIntent(
            schemaVersion = value.getInt("schemaVersion"),
            previousState = value.optJSONObject("previousState")?.let(HandoffState::fromJson),
            previousActiveSha256 = value.optNullableString("previousActiveSha256"),
            nextState = HandoffState.fromJson(value.getJSONObject("nextState")),
        ).also { it.validate(documentId) }
    }
}
data class RetiredPredecessorWatch(
    val schemaVersion: Int = 1,
    val previousState: HandoffState,
    val retiredFileName: String,
    val observedSha256: String,
    val localGeneration: Long,
) {
    fun validate(documentId: String) {
        require(schemaVersion == 1) { "Unsupported retired predecessor watch version" }
        previousState.validate()
        require(previousState.retiredPredecessors.isEmpty()) {
            "Retired predecessor watches cannot contain nested watch history"
        }
        require(previousState.documentId == documentId) { "Retired predecessor watch belongs elsewhere" }
        requireSafeFileName(retiredFileName, "retired predecessor file name")
        require(retiredFileName == previousState.activeFileName) {
            "Retired predecessor watch does not match its handoff state"
        }
        require(SHA256.matches(observedSha256)) { "Invalid retired predecessor hash" }
        require(localGeneration >= previousState.localGeneration) {
            "Retired predecessor generation predates its handoff state"
        }
    }

    fun toJson(): JSONObject = JSONObject()
        .put("schemaVersion", schemaVersion)
        .put("previousState", previousState.toJson())
        .put("retiredFileName", retiredFileName)
        .put("observedSha256", observedSha256)
        .put("localGeneration", localGeneration)

    companion object {
        fun fromJson(value: JSONObject, documentId: String) = RetiredPredecessorWatch(
            schemaVersion = value.getInt("schemaVersion"),
            previousState = HandoffState.fromJson(value.getJSONObject("previousState")),
            retiredFileName = value.getString("retiredFileName"),
            observedSha256 = value.getString("observedSha256"),
            localGeneration = value.getLong("localGeneration"),
        ).also { it.validate(documentId) }
    }
}

data class FinalizeIntent(
    val schemaVersion: Int = 1,
    val previousState: HandoffState,
    val nextState: HandoffState,
) {
    fun validate(documentId: String) {
        require(schemaVersion == 1) { "Unsupported finalize intent version" }
        previousState.validate()
        nextState.validate()
        require(previousState.documentId == documentId && nextState.documentId == documentId) {
            "Finalize intent belongs to a different document"
        }
        val finalizedHash = requireNotNull(nextState.finalizedLocalSha256) {
            "Finalize intent is missing its content hash"
        }
        val outputName = requireNotNull(nextState.finalizedOutputFileName) {
            "Finalize intent is missing its output file name"
        }
        require(
            nextState == previousState.copy(
                finalizedLocalSha256 = finalizedHash,
                finalizedOutputFileName = outputName,
                localGeneration = previousState.localGeneration + 1,
            ),
        ) { "Finalize intent does not describe one durable BOOX snapshot" }
    }

    fun toJson(): JSONObject = JSONObject()
        .put("schemaVersion", schemaVersion)
        .put("previousState", previousState.toJson())
        .put("nextState", nextState.toJson())

    companion object {
        fun fromJson(value: JSONObject, documentId: String) = FinalizeIntent(
            schemaVersion = value.getInt("schemaVersion"),
            previousState = HandoffState.fromJson(value.getJSONObject("previousState")),
            nextState = HandoffState.fromJson(value.getJSONObject("nextState")),
        ).also { it.validate(documentId) }
    }
}

sealed class InstallDecision {
    data object Install : InstallDecision()
    data object Duplicate : InstallDecision()
    data class Reject(val reason: String) : InstallDecision()
}

object HandoffPolicy {
    fun decideInstall(
        state: HandoffState?,
        delivery: BrokerDelivery,
        activeSha256: String?,
    ): InstallDecision {
        delivery.validate()
        if (state == null) return InstallDecision.Install
        if (state.documentId != delivery.documentId) {
            return InstallDecision.Reject("The delivery belongs to a different document")
        }
        if (delivery.eventId in state.processedEventIds) return InstallDecision.Duplicate
        if (delivery.sourceRevisions == state.activeRevisions) {
            return if (delivery.contentSha256 == state.installedBrokerSha256) {
                InstallDecision.Duplicate
            } else {
                InstallDecision.Reject("Same revision has different PDF content")
            }
        }
        if (!delivery.sourceRevisions.strictlyDominates(state.activeRevisions)) {
            return InstallDecision.Reject("Delivery is stale or conflicts with the active revisions")
        }
        if (delivery.sourceGeneration <= state.sourceGeneration) {
            return InstallDecision.Reject("Delivery generation is not newer")
        }
        if (activeSha256 == null) return InstallDecision.Reject("The active PDF is missing")
        if (activeSha256 != state.installedBrokerSha256) {
            if (activeSha256 != state.finalizedLocalSha256) {
                return InstallDecision.Reject("Finalize the current BOOX changes before installing an update")
            }
            if (delivery.sourceRevisions.boox <= state.activeRevisions.boox) {
                return InstallDecision.Reject("The broker has not accepted the finalized BOOX revision yet")
            }
        }
        return InstallDecision.Install
    }
}

internal fun sha256Hex(bytes: ByteArray): String =
    MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

internal fun sha256Hex(file: File): String {
    val digest = MessageDigest.getInstance("SHA-256")
    file.inputStream().buffered().use { input ->
        val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
        while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            if (count > 0) digest.update(buffer, 0, count)
        }
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}

internal fun requireSafeFileName(value: String, label: String) {
    require(value.isNotBlank() && value.toByteArray(Charsets.UTF_8).size <= SAFE_FILE_NAME_MAX_BYTES) { "Invalid $label" }
    require(value != "." && value != "..") { "Invalid $label" }
    require(value.none { character ->
        character == '/' || character == '\\' || Character.isISOControl(character)
    }) { "Invalid $label" }
}

private fun JSONObject.optNullableString(key: String): String? =
    if (isNull(key)) null else optString(key).takeIf { it.isNotBlank() }

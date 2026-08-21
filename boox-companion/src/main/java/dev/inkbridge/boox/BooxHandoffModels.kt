package dev.inkbridge.boox

import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.security.MessageDigest

internal const val BROKER_PRODUCER = "inkbridge-broker"
internal const val DESCRIPTOR_SCHEMA_VERSION = 1
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
) {
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

    companion object {
        fun fromJson(value: JSONObject): HandoffState {
            val eventIds = value.optJSONArray("processedEventIds") ?: JSONArray()
            return HandoffState(
                schemaVersion = value.getInt("schemaVersion"),
                documentId = value.getString("documentId"),
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
            )
        }
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
    require(value.isNotBlank() && value.toByteArray(Charsets.UTF_8).size <= 180) { "Invalid $label" }
    require(value != "." && value != "..") { "Invalid $label" }
    require(value.none { character ->
        character == '/' || character == '\\' || Character.isISOControl(character)
    }) { "Invalid $label" }
}

private fun JSONObject.optNullableString(key: String): String? =
    if (isNull(key)) null else optString(key).takeIf { it.isNotBlank() }

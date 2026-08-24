package dev.inkbridge.boox

import org.json.JSONObject
import java.io.File

internal sealed class CompactFinalizeResult {
    data object NoChanges : CompactFinalizeResult()
    data class Finalized(
        val manifest: File,
        val descriptor: File,
        val state: HandoffState,
        val operationCount: Int,
    ) : CompactFinalizeResult()
    data class AlreadyFinalized(val manifest: File?) : CompactFinalizeResult()
    data class FullPdfFallbackRequired(val reason: String) : CompactFinalizeResult()
}

internal class CompactBooxFinalizer(
    private val store: BooxHandoffStore,
    private val converter: BooxManifestConverter,
) {
    fun prepareBaseline(state: HandoffState): File {
        val active = store.activeFile(state)
        require(active.isFile) { "The active PDF is missing" }
        val baseline = baselineFile(state)
        if (baseline.isFile) return baseline
        require(sha256Hex(active) == state.installedBrokerSha256) {
            "The active PDF changed before its compact baseline was prepared"
        }
        val bytes = converter.buildBaseline(active, state.originalFileName)
        require(bytes.isNotEmpty()) { "The native converter returned an empty BOOX baseline" }
        require(bytes.size.toLong() <= MAX_COMPACT_BASELINE_BYTES) {
            "The compact BOOX baseline is unreasonably large"
        }
        store.publishPayloadBytesOrVerify(bytes, baseline, sha256Hex(bytes))
        return baseline
    }

    fun finalize(documentId: String): CompactFinalizeResult {
        val state = store.state(documentId) ?: error("No active InkBridge document")
        val active = store.activeFile(state)
        require(active.isFile) { "The active PDF is missing" }
        val currentHash = sha256Hex(active)
        state.finalizedLocalSha256?.let { finalizedHash ->
            check(currentHash == finalizedHash) {
                "Wait for the previous finalized BOOX changes to be acknowledged before finalizing again"
            }
            return CompactFinalizeResult.AlreadyFinalized(
                state.finalizedOutputFileName?.let { File(store.outgoingDirectory(documentId), it) },
            )
        }
        if (currentHash == state.installedBrokerSha256) return CompactFinalizeResult.NoChanges

        val baseline = baselineFile(state)
        if (!baseline.isFile) {
            return CompactFinalizeResult.FullPdfFallbackRequired(
                "The compact baseline is missing; use Finalize BOOX changes for this revision",
            )
        }
        if (baseline.length() > MAX_COMPACT_BASELINE_BYTES) {
            return CompactFinalizeResult.FullPdfFallbackRequired(
                "The compact baseline is too large; use Finalize BOOX changes for this revision",
            )
        }
        val converted = runCatching {
            val baselineBytes = baseline.readBytes()
            validateBaseline(baselineBytes, state)
            val bytes = converter.buildManifest(active, baselineBytes)
            bytes to validateManifest(bytes, currentHash, state.originalFileName)
        }.getOrElse { error ->
            return CompactFinalizeResult.FullPdfFallbackRequired(
                "On-device conversion failed: " +
                    (error.message ?: error.javaClass.simpleName),
            )
        }
        val (manifestBytes, operationCount) = converted
        val payloadHash = sha256Hex(manifestBytes)
        val localGeneration = Math.addExact(state.localGeneration, 1)
        val outputName =
            "boox-g" + localGeneration + "-" + currentHash.take(12) + ".operations.json"
        val outgoing = store.outgoingDirectory(documentId)
        val manifest = File(outgoing, outputName)
        val descriptor = File(outgoing, "$outputName.inkbridge.json")
        val eventIdentity = listOf(
            state.documentId,
            state.activeRevisions.boox,
            state.activeRevisions.supernote,
            state.activeRevisions.boox + 1,
            currentHash,
            payloadHash,
        ).joinToString(":")
        val eventId = "boox-compact-finalize-" + sha256Hex(eventIdentity.toByteArray())
        val descriptorBytes = JSONObject()
            .put("schemaVersion", 1)
            .put("eventId", eventId)
            .put("documentId", state.documentId)
            .put("source", "boox")
            .put("objectPath", "BOOX_Folder/" + state.documentId + "/" + outputName)
            .put("sourceGeneration", localGeneration)
            .put("sourceRevision", state.activeRevisions.boox + 1)
            .put("basedOn", state.activeRevisions.toJson())
            .put("contentSha256", payloadHash)
            .put("payloadKind", "boox_operation_manifest")
            .toString(2)
            .toByteArray()

        store.publishPayloadBytesOrVerify(manifestBytes, manifest, payloadHash)
        store.publishDescriptorBytesOrVerify(descriptorBytes, descriptor)
        val committed = store.recordCompactFinalization(
            expected = state,
            activeSha256 = currentHash,
            outputName = outputName,
        )
        return CompactFinalizeResult.Finalized(
            manifest = manifest,
            descriptor = descriptor,
            state = committed,
            operationCount = operationCount,
        )
    }

    private fun baselineFile(state: HandoffState): File =
        File(
            store.documentDirectory(state.documentId),
            ".inkbridge-baseline-" + state.installedBrokerSha256.take(16) + ".json",
        )

    private fun validateBaseline(bytes: ByteArray, state: HandoffState) {
        val baseline = JSONObject(String(bytes, Charsets.UTF_8))
        require(baseline.getInt("schemaVersion") == 1) {
            "The native converter returned an unsupported baseline"
        }
        require(baseline.getString("pdfSha256") == state.installedBrokerSha256) {
            "The compact baseline does not match the installed broker PDF"
        }
        require(baseline.getString("sourceFileName") == state.originalFileName) {
            "The compact baseline does not match the logical document name"
        }
    }

    private fun validateManifest(
        bytes: ByteArray,
        activeSha256: String,
        sourceFileName: String,
    ): Int {
        val manifest = JSONObject(String(bytes, Charsets.UTF_8))
        require(manifest.getInt("schemaVersion") == 1) {
            "The native converter returned an unsupported manifest"
        }
        val document = manifest.getJSONObject("document")
        require(document.getString("pdfSha256") == activeSha256) {
            "The native converter manifest does not match the closed NeoReader PDF"
        }
        require(document.getString("sourceFileName") == sourceFileName) {
            "The native converter manifest does not match the logical document name"
        }
        return manifest.getJSONArray("operations").length()
    }

    private companion object {
        const val MAX_COMPACT_BASELINE_BYTES = 64L * 1024L * 1024L
    }
}

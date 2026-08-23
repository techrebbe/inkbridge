package dev.inkbridge.boox

import org.json.JSONArray
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class BooxHandoffModelsTest {
    @Test
    fun legacyReplayHistoryIsTrimmedToTheSafeTail() {
        val eventIds = List(MAX_PROCESSED_EVENT_IDS + 5) { "event-$it" }
        val json = state().toJson().put("processedEventIds", JSONArray(eventIds))

        val restored = HandoffState.fromJson(json)

        assertEquals(eventIds.takeLast(MAX_PROCESSED_EVENT_IDS), restored.processedEventIds)
    }

    @Test
    fun installedAcknowledgementRoundTripsFromCommittedState() {
        val state = state()
        val acknowledgement = InstalledDeliveryAcknowledgement.fromState(state)

        assertEquals(
            acknowledgement,
            InstalledDeliveryAcknowledgement.fromJson(acknowledgement.toJson()),
        )
        assertEquals(state.brokerEventId, acknowledgement.eventId)
        assertEquals(state.activeRevisions, acknowledgement.sourceRevisions)
        assertEquals(state.installedBrokerSha256, acknowledgement.contentSha256)
    }
    @Test
    fun worstCaseReplayHistoryFitsInstallAndFinalizeIntents() {
        val worstCaseEventId = "\u0000".repeat(256)
        val replayHistory = List(MAX_PROCESSED_EVENT_IDS) { worstCaseEventId }
        val retiredState = state(
            brokerEventId = worstCaseEventId,
            activeFileName = "retired.pdf",
        ).copy(openedBrokerEventId = worstCaseEventId)
        val retired = RetiredPredecessorWatch(
            previousState = retiredState,
            retiredFileName = retiredState.activeFileName,
            observedSha256 = retiredState.installedBrokerSha256,
            localGeneration = retiredState.localGeneration,
        )
        val previous = state(
            brokerEventId = worstCaseEventId,
            activeFileName = "previous.pdf",
        ).copy(
            processedEventIds = replayHistory,
            retiredPredecessors = listOf(retired),
            openedBrokerEventId = worstCaseEventId,
        )
        val next = previous.copy(
            activeRevisions = RevisionPair(2, 1),
            sourceGeneration = 2,
            activeFileName = "next.pdf",
            installedBrokerSha256 = "b".repeat(64),
            openedBrokerEventId = null,
        )
        val finalized = previous.copy(
            finalizedLocalSha256 = "c".repeat(64),
            finalizedOutputFileName = "previous__boox-finalized-g1.pdf",
            localGeneration = 1,
        )
        val installIntent = InstallIntent(
            previousState = previous,
            previousActiveSha256 = previous.installedBrokerSha256,
            nextState = next,
        ).also { it.validate(previous.documentId) }
        val finalizeIntent = FinalizeIntent(
            previousState = previous,
            nextState = finalized,
        ).also { it.validate(previous.documentId) }

        assertTrue(installIntent.toJson().toString(2).toByteArray().size <= MAX_DESCRIPTOR_BYTES)
        assertTrue(finalizeIntent.toJson().toString(2).toByteArray().size <= MAX_DESCRIPTOR_BYTES)
    }

    private fun state(
        brokerEventId: String = "event-1",
        activeFileName: String = "active.pdf",
    ) = HandoffState(
        documentId = "inkbridge-doc-v1-" + "a".repeat(64),
        originalFileName = "Example.pdf",
        activeRevisions = RevisionPair(1, 1),
        sourceGeneration = 1,
        brokerEventId = brokerEventId,
        activeFileName = activeFileName,
        installedBrokerSha256 = "a".repeat(64),
    )
}

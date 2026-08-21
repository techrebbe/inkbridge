package dev.inkbridge.boox

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class NeoReaderHandoffTrackerTest {
    @Test
    fun confirmsOnlyAfterActivityPausedAndResumed() {
        val persistence = MemoryHandoffPersistence()
        val tracker = NeoReaderHandoffTracker(persistence)
        val state = state()

        tracker.launchStarted(state)
        assertNull(tracker.activityResumed())
        tracker.activityPaused()
        assertEquals(expected(state), tracker.activityResumed())
        assertNull(tracker.activityResumed())
        assertNull(persistence.value)
    }

    @Test
    fun failedLaunchNeverConfirms() {
        val persistence = MemoryHandoffPersistence()
        val tracker = NeoReaderHandoffTracker(persistence)

        tracker.launchStarted(state())
        tracker.launchFailed()
        tracker.activityPaused()

        assertNull(tracker.activityResumed())
        assertNull(persistence.value)
    }

    @Test
    fun pausedHandoffSurvivesTrackerRecreation() {
        val persistence = MemoryHandoffPersistence()
        val state = state()
        NeoReaderHandoffTracker(persistence).apply {
            launchStarted(state)
            activityPaused()
        }

        val recreated = NeoReaderHandoffTracker(persistence)

        assertEquals(expected(state), recreated.activityResumed())
        assertNull(persistence.value)
    }

    private fun expected(state: HandoffState) = PendingNeoReaderHandoff(
        documentId = state.documentId,
        brokerEventId = state.brokerEventId,
        activeFileName = state.activeFileName,
        observedPause = true,
    )

    private fun state() = HandoffState(
        documentId = "inkbridge-doc-v1-" + "a".repeat(64),
        originalFileName = "Example.pdf",
        activeRevisions = RevisionPair(1, 2),
        sourceGeneration = 3,
        brokerEventId = "event-3",
        activeFileName = "Example__ib-b1-s2-g3.pdf",
        installedBrokerSha256 = "b".repeat(64),
    )

    private class MemoryHandoffPersistence(
        var value: PendingNeoReaderHandoff? = null,
    ) : NeoReaderHandoffPersistence {
        override fun load(): PendingNeoReaderHandoff? = value

        override fun save(value: PendingNeoReaderHandoff?) {
            this.value = value
        }
    }
}

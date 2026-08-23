package dev.inkbridge.boox

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class NeoReaderHandoffTrackerTest {
    @Test
    fun confirmsOnlyAfterDispatchedActivityPausedAndStorageCommit() {
        val persistence = MemoryHandoffPersistence()
        val tracker = NeoReaderHandoffTracker(persistence)
        val state = state()

        tracker.launchStarted(state)
        assertNull(tracker.activityResumed())
        tracker.activityPaused()
        assertNull(tracker.activityResumed())
        tracker.launchDispatched()
        tracker.activityPaused()
        val opened = tracker.activityResumed()
        assertEquals(expected(state), opened)
        assertNotNull(persistence.value)

        tracker.confirmationCommitted(requireNotNull(opened))

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
    fun pausedHandoffSurvivesTrackerRecreationUntilConfirmationCommits() {
        val persistence = MemoryHandoffPersistence()
        val state = state()
        NeoReaderHandoffTracker(persistence).apply {
            launchStarted(state)
            launchDispatched()
            activityPaused()
        }

        val recreated = NeoReaderHandoffTracker(persistence)
        val opened = recreated.activityResumed()
        assertEquals(expected(state), opened)
        assertNotNull(persistence.value)

        val recreatedBeforeCommit = NeoReaderHandoffTracker(persistence)
        val recoveredAgain = recreatedBeforeCommit.activityResumed()
        assertEquals(expected(state), recoveredAgain)
        recreatedBeforeCommit.confirmationCommitted(requireNotNull(recoveredAgain))

        assertNull(persistence.value)
    }

    @Test
    fun dispatchedLaunchWithoutItsOwnPauseCannotBeConfirmedAfterProcessRestart() {
        val persistence = MemoryHandoffPersistence()
        NeoReaderHandoffTracker(persistence).apply {
            launchStarted(state())
            launchDispatched()
        }

        val recreated = NeoReaderHandoffTracker(persistence)
        recreated.activityPaused()

        assertNull(recreated.activityResumed())
        assertEquals(false, persistence.value?.observedPause)
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

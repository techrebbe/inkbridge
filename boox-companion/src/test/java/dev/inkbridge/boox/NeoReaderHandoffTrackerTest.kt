package dev.inkbridge.boox

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class NeoReaderHandoffTrackerTest {
    @Test
    fun confirmsOnlyAfterActivityPausedAndResumed() {
        val tracker = NeoReaderHandoffTracker()
        val state = state()

        tracker.launchStarted(state)
        assertNull(tracker.activityResumed())
        tracker.activityPaused()
        assertEquals(state, tracker.activityResumed())
        assertNull(tracker.activityResumed())
    }

    @Test
    fun failedLaunchNeverConfirms() {
        val tracker = NeoReaderHandoffTracker()
        tracker.launchStarted(state())
        tracker.launchFailed()
        tracker.activityPaused()
        assertNull(tracker.activityResumed())
    }

    private fun state() = HandoffState(
        documentId = "inkbridge-doc-v1-" + "a".repeat(64),
        originalFileName = "Example.pdf",
        activeRevisions = RevisionPair(1, 2),
        sourceGeneration = 3,
        brokerEventId = "event-3",
        activeFileName = "Example__ib-b1-s2-g3.pdf",
        installedBrokerSha256 = "b".repeat(64),
    )
}
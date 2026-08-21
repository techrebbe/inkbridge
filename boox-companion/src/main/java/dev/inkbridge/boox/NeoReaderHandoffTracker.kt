package dev.inkbridge.boox

import android.content.SharedPreferences

internal data class PendingNeoReaderHandoff(
    val documentId: String,
    val brokerEventId: String,
    val activeFileName: String,
    val observedPause: Boolean,
)

internal interface NeoReaderHandoffPersistence {
    fun load(): PendingNeoReaderHandoff?
    fun save(value: PendingNeoReaderHandoff?)
}

internal class SharedPreferencesNeoReaderHandoffPersistence(
    private val preferences: SharedPreferences,
) : NeoReaderHandoffPersistence {
    override fun load(): PendingNeoReaderHandoff? {
        val documentId = preferences.getString(KEY_DOCUMENT_ID, null)
        val brokerEventId = preferences.getString(KEY_BROKER_EVENT_ID, null)
        val activeFileName = preferences.getString(KEY_ACTIVE_FILE_NAME, null)
        if (documentId.isNullOrBlank() || brokerEventId.isNullOrBlank() || activeFileName.isNullOrBlank()) {
            save(null)
            return null
        }
        return PendingNeoReaderHandoff(
            documentId = documentId,
            brokerEventId = brokerEventId,
            activeFileName = activeFileName,
            observedPause = preferences.getBoolean(KEY_OBSERVED_PAUSE, false),
        )
    }

    override fun save(value: PendingNeoReaderHandoff?) {
        val editor = preferences.edit().clear()
        if (value != null) {
            editor
                .putString(KEY_DOCUMENT_ID, value.documentId)
                .putString(KEY_BROKER_EVENT_ID, value.brokerEventId)
                .putString(KEY_ACTIVE_FILE_NAME, value.activeFileName)
                .putBoolean(KEY_OBSERVED_PAUSE, value.observedPause)
        }
        check(editor.commit()) { "Could not persist the pending NeoReader handoff" }
    }

    private companion object {
        const val KEY_DOCUMENT_ID = "document-id"
        const val KEY_BROKER_EVENT_ID = "broker-event-id"
        const val KEY_ACTIVE_FILE_NAME = "active-file-name"
        const val KEY_OBSERVED_PAUSE = "observed-pause"
    }
}

internal class NeoReaderHandoffTracker(
    private val persistence: NeoReaderHandoffPersistence,
) {
    private var pending = persistence.load()

    fun launchStarted(state: HandoffState) {
        update(
            PendingNeoReaderHandoff(
                documentId = state.documentId,
                brokerEventId = state.brokerEventId,
                activeFileName = state.activeFileName,
                observedPause = false,
            ),
        )
    }

    fun launchFailed() {
        update(null)
    }

    fun activityPaused() {
        val current = pending ?: return
        if (!current.observedPause) update(current.copy(observedPause = true))
    }

    fun activityResumed(): PendingNeoReaderHandoff? =
        pending?.takeIf(PendingNeoReaderHandoff::observedPause)

    fun confirmationCommitted(opened: PendingNeoReaderHandoff) {
        check(pending == opened) { "NeoReader handoff confirmation does not match the pending launch" }
        update(null)
    }

    private fun update(value: PendingNeoReaderHandoff?) {
        persistence.save(value)
        pending = value
    }
}

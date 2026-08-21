package dev.inkbridge.boox

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.net.Uri
import android.os.Bundle
import android.os.Environment
import android.os.StrictMode
import android.provider.Settings
import android.util.Log
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.io.File
import java.util.concurrent.Executors

class BooxHandoffActivity : Activity() {
    private lateinit var store: BooxHandoffStore
    private lateinit var status: TextView
    private val actionButtons = mutableListOf<Button>()
    private var operationRunning = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = BooxHandoffStore(File(Environment.getExternalStorageDirectory(), "Documents/InkBridge"))
        setContentView(buildUi())
        renderStatus("Ready")
        dispatchIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        dispatchIntent(intent)
    }

    private fun buildUi(): View {
        val content = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(36, 40, 36, 40)
            gravity = Gravity.CENTER_HORIZONTAL
        }
        content.addView(TextView(this).apply {
            text = "InkBridge BOOX"
            textSize = 28f
            setTextColor(0xff111111.toInt())
        }, matchWrap())
        content.addView(TextView(this).apply {
            text = "Safe handoff to and from NeoReader"
            textSize = 16f
            setPadding(0, 8, 0, 28)
        }, matchWrap())
        content.addButton("Install next update") { installNext() }
        content.addButton("Open active document in NeoReader") {
            chooseActiveDocument("Open which document?") { openActive(it) }
        }
        content.addButton("Finalize BOOX changes") {
            chooseActiveDocument("Finalize which document?") { finalizeActive(it) }
        }
        content.addButton("Grant file access") { requestStorageAccess() }
        status = TextView(this).apply {
            textSize = 16f
            setTextColor(0xff111111.toInt())
            setPadding(0, 28, 0, 0)
        }
        content.addView(status, matchWrap())
        return ScrollView(this).apply { addView(content) }
    }

    private fun LinearLayout.addButton(label: String, action: () -> Unit) {
        val button = Button(this@BooxHandoffActivity).apply {
            text = label
            textSize = 17f
            isAllCaps = false
            setOnClickListener { action() }
        }
        actionButtons += button
        addView(button, LinearLayout.LayoutParams(-1, -2).apply { bottomMargin = 18 })
    }

    private fun dispatchIntent(intent: Intent) {
        if (applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE == 0) return
        when (intent.action) {
            ACTION_INSTALL_NEXT -> installNext()
            ACTION_OPEN_ACTIVE -> openActive(intent.getStringExtra(EXTRA_DOCUMENT_ID))
            ACTION_FINALIZE_ACTIVE -> finalizeActive(intent.getStringExtra(EXTRA_DOCUMENT_ID))
        }
    }

    private fun installNext() = runStorageAction("Installing next broker update...") {
        requireStorageAccess()
        val descriptor = store.findNextDescriptor() ?: error("No new broker update is waiting")
        when (val result = store.install(descriptor)) {
            is InstallResult.Installed -> StorageOutcome(
                "Installed revision b" + result.state.activeRevisions.boox +
                    " / s" + result.state.activeRevisions.supernote +
                    "\n" + result.activeFile.name,
                result.state,
            )
            is InstallResult.Duplicate -> StorageOutcome(
                "That broker update was already installed",
                store.findMostRecentState(),
            )
        }
    }

    private fun chooseActiveDocument(title: String, action: (String) -> Unit) {
        if (operationRunning) {
            Log.i(TAG, "IGNORED document selection while another storage operation is running")
            return
        }
        operationRunning = true
        setActionsEnabled(false)
        renderStatus("Loading active documents...")
        STORAGE_EXECUTOR.execute {
            val result = runCatching {
                requireStorageAccess()
                store.activeDocumentCatalog()
            }
            runOnUiThread {
                if (isDestroyed || isFinishing) return@runOnUiThread
                operationRunning = false
                setActionsEnabled(true)
                result.fold(
                    onSuccess = { catalog ->
                        when {
                            catalog.states.isEmpty() && catalog.failures.isNotEmpty() -> {
                                val failure = catalog.failures.first()
                                showFailure(
                                    IllegalStateException(
                                        "No accessible active document; " +
                                            failure.documentId.takeLast(8) + ": " + failure.message,
                                    ),
                                )
                            }
                            catalog.states.isEmpty() -> {
                                showFailure(IllegalStateException("No active InkBridge document"))
                            }
                            catalog.states.size == 1 && catalog.failures.isEmpty() -> {
                                action(catalog.states.single().documentId)
                            }
                            else -> showDocumentPicker(title, catalog, action)
                        }
                    },
                    onFailure = ::showFailure,
                )
            }
        }
    }

    private fun showDocumentPicker(
        title: String,
        catalog: ActiveDocumentCatalog,
        action: (String) -> Unit,
    ) {
        val labels = catalog.states.map { state ->
            state.originalFileName + "\nb" + state.activeRevisions.boox +
                " / s" + state.activeRevisions.supernote + " - " + state.documentId.takeLast(8)
        }.toTypedArray()
        val unavailable = catalog.failures.size
        renderStatus(
            "Choose an active document" +
                if (unavailable == 0) "" else "\n$unavailable damaged document(s) unavailable",
        )
        AlertDialog.Builder(this)
            .setTitle(title)
            .setItems(labels) { _, index -> action(catalog.states[index].documentId) }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun openActive(documentId: String? = null) =
        runStorageAction("Preparing the active document...") {
            requireStorageAccess()
            val state = documentId?.let(store::state) ?: store.findMostRecentState()
                ?: error("No active InkBridge document")
            val pdf = File(File(File(store.root, state.documentId), "active"), state.activeFileName)
            require(pdf.isFile) { "The active PDF is missing" }
            StorageOutcome("Opening active document in NeoReader", state, pdf)
        }

    private fun finalizeActive(documentId: String? = null) =
        runStorageAction("Finalizing BOOX changes...") {
            requireStorageAccess()
            val state = documentId?.let(store::state) ?: store.findMostRecentState()
                ?: error("No active InkBridge document")
            when (val result = store.finalize(state.documentId)) {
                FinalizeResult.NoChanges -> StorageOutcome(
                    "No new BOOX changes to finalize",
                    state,
                )
                is FinalizeResult.AlreadyFinalized -> StorageOutcome(
                    "These BOOX changes were already finalized",
                    store.state(state.documentId),
                )
                is FinalizeResult.Finalized -> StorageOutcome(
                    "BOOX changes finalized\n" + result.pdf.name + "\nReady for folder sync",
                    result.state,
                )
            }
        }

    private fun requestStorageAccess() = runUiAction {
        if (Environment.isExternalStorageManager()) {
            renderStatus("File access is already enabled")
            return@runUiAction
        }
        startActivity(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION).apply {
            data = Uri.parse("package:$packageName")
        })
    }

    private fun requireStorageAccess() {
        check(Environment.isExternalStorageManager()) { "Tap Grant file access first" }
    }

    private fun runStorageAction(progress: String, action: () -> StorageOutcome) {
        if (operationRunning) {
            Log.i(TAG, "IGNORED action while another storage operation is running")
            return
        }
        operationRunning = true
        setActionsEnabled(false)
        renderStatus(progress)
        STORAGE_EXECUTOR.execute {
            val result = runCatching(action)
            runOnUiThread {
                if (isDestroyed || isFinishing) return@runOnUiThread
                operationRunning = false
                setActionsEnabled(true)
                result.fold(
                    onSuccess = { outcome ->
                        runCatching { applyOutcome(outcome) }
                            .onFailure(::showFailure)
                    },
                    onFailure = ::showFailure,
                )
            }
        }
    }

    private fun applyOutcome(outcome: StorageOutcome) {
        renderStatus(outcome.message, outcome.recent)
        val pdf = outcome.openPdf ?: return
        val state = requireNotNull(outcome.recent)
        // NeoReader explicitly advertises file:// PDF intents. This narrowly-scoped policy avoids
        // FileUriExposedException while preserving in-place editable annotation behavior.
        StrictMode.setVmPolicy(StrictMode.VmPolicy.Builder().build())
        startActivity(Intent(Intent.ACTION_VIEW).apply {
            setClassName("com.onyx.kreader", "com.onyx.kreader.ui.ReaderHomeActivity")
            setDataAndType(Uri.fromFile(pdf), "application/pdf")
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        })
        Log.i(TAG, "OPEN document=" + state.documentId + " file=" + pdf.absolutePath)
    }

    private fun runUiAction(action: () -> Unit) {
        runCatching(action).onFailure(::showFailure)
    }

    private fun showFailure(error: Throwable) {
        Log.e(TAG, "ERROR " + error.message, error)
        renderStatus("Stopped safely: " + error.message)
    }

    private fun setActionsEnabled(enabled: Boolean) {
        actionButtons.forEach { it.isEnabled = enabled }
    }

    private fun renderStatus(message: String, recent: HandoffState? = null) {
        val detail = recent?.let {
            "\n\nActive: b" + it.activeRevisions.boox + " / s" +
                it.activeRevisions.supernote + "\n" + it.originalFileName
        }.orEmpty()
        status.text = message + detail
        Log.i(
            TAG,
            message + (recent?.let { " document=" + it.documentId }.orEmpty()),
        )
    }

    private fun matchWrap() = LinearLayout.LayoutParams(-1, -2)

    private data class StorageOutcome(
        val message: String,
        val recent: HandoffState? = null,
        val openPdf: File? = null,
    )

    companion object {
        private val STORAGE_EXECUTOR = Executors.newSingleThreadExecutor()
        private const val TAG = "INKBRIDGE_BOOX_HANDOFF"
        private const val EXTRA_DOCUMENT_ID = "documentId"
        private const val ACTION_INSTALL_NEXT = "dev.inkbridge.boox.action.INSTALL_NEXT"
        private const val ACTION_OPEN_ACTIVE = "dev.inkbridge.boox.action.OPEN_ACTIVE"
        private const val ACTION_FINALIZE_ACTIVE = "dev.inkbridge.boox.action.FINALIZE_ACTIVE"
    }
}

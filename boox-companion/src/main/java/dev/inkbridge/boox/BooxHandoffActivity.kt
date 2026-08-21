package dev.inkbridge.boox

import android.app.Activity
import android.content.Intent
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

class BooxHandoffActivity : Activity() {
    private lateinit var store: BooxHandoffStore
    private lateinit var status: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = BooxHandoffStore(File(Environment.getExternalStorageDirectory(), "Documents/InkBridge"))
        setContentView(buildUi())
        refreshStatus("Ready")
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
        content.addButton("Open active document in NeoReader") { openActive() }
        content.addButton("Finalize BOOX changes") { finalizeActive() }
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
        addView(Button(this@BooxHandoffActivity).apply {
            text = label
            textSize = 17f
            isAllCaps = false
            setOnClickListener { runAction(action) }
        }, LinearLayout.LayoutParams(-1, -2).apply { bottomMargin = 18 })
    }

    private fun dispatchIntent(intent: Intent) {
        when (intent.action) {
            ACTION_INSTALL_NEXT -> installNext()
            ACTION_OPEN_ACTIVE -> openActive(intent.getStringExtra(EXTRA_DOCUMENT_ID))
            ACTION_FINALIZE_ACTIVE -> finalizeActive(intent.getStringExtra(EXTRA_DOCUMENT_ID))
        }
    }

    private fun installNext() = runAction {
        requireStorageAccess()
        val descriptor = store.findNextDescriptor() ?: error("No new broker update is waiting")
        when (val result = store.install(descriptor)) {
            is InstallResult.Installed -> refreshStatus(
                "Installed revision b${result.state.activeRevisions.boox} / " +
                    "s${result.state.activeRevisions.supernote}\n${result.activeFile.name}",
            )
            is InstallResult.Duplicate -> refreshStatus("That broker update was already installed")
        }
    }

    private fun openActive(documentId: String? = null) = runAction {
        requireStorageAccess()
        val state = documentId?.let(store::state) ?: store.findMostRecentState()
            ?: error("No active InkBridge document")
        val pdf = File(File(File(store.root, state.documentId), "active"), state.activeFileName)
        require(pdf.isFile) { "The active PDF is missing" }
        // NeoReader explicitly advertises file:// PDF intents. This narrowly-scoped policy avoids
        // FileUriExposedException while preserving in-place editable annotation behavior.
        StrictMode.setVmPolicy(StrictMode.VmPolicy.Builder().build())
        startActivity(Intent(Intent.ACTION_VIEW).apply {
            setClassName("com.onyx.kreader", "com.onyx.kreader.ui.ReaderHomeActivity")
            setDataAndType(Uri.fromFile(pdf), "application/pdf")
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        })
        Log.i(TAG, "OPEN document=${state.documentId} file=${pdf.absolutePath}")
    }

    private fun finalizeActive(documentId: String? = null) = runAction {
        requireStorageAccess()
        val state = documentId?.let(store::state) ?: store.findMostRecentState()
            ?: error("No active InkBridge document")
        when (val result = store.finalize(state.documentId)) {
            FinalizeResult.NoChanges -> refreshStatus("No new BOOX changes to finalize")
            is FinalizeResult.AlreadyFinalized -> refreshStatus("These BOOX changes were already finalized")
            is FinalizeResult.Finalized -> refreshStatus(
                "BOOX changes finalized\n${result.pdf.name}\nReady for folder sync",
            )
        }
    }

    private fun requestStorageAccess() {
        if (Environment.isExternalStorageManager()) {
            refreshStatus("File access is already enabled")
            return
        }
        startActivity(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION).apply {
            data = Uri.parse("package:$packageName")
        })
    }

    private fun requireStorageAccess() {
        check(Environment.isExternalStorageManager()) { "Tap Grant file access first" }
    }

    private fun runAction(action: () -> Unit) {
        runCatching(action).onFailure {
            Log.e(TAG, "ERROR ${it.message}", it)
            refreshStatus("Stopped safely: ${it.message}")
        }
    }

    private fun refreshStatus(message: String) {
        val recent = runCatching { store.findMostRecentState() }.getOrNull()
        val detail = recent?.let {
            "\n\nActive: b${it.activeRevisions.boox} / s${it.activeRevisions.supernote}" +
                "\n${it.originalFileName}"
        }.orEmpty()
        status.text = message + detail
        Log.i(TAG, "$message${recent?.let { " document=${it.documentId}" }.orEmpty()}")
    }

    private fun matchWrap() = LinearLayout.LayoutParams(-1, -2)

    companion object {
        private const val TAG = "INKBRIDGE_BOOX_HANDOFF"
        private const val EXTRA_DOCUMENT_ID = "documentId"
        private const val ACTION_INSTALL_NEXT = "dev.inkbridge.boox.action.INSTALL_NEXT"
        private const val ACTION_OPEN_ACTIVE = "dev.inkbridge.boox.action.OPEN_ACTIVE"
        private const val ACTION_FINALIZE_ACTIVE = "dev.inkbridge.boox.action.FINALIZE_ACTIVE"
    }
}

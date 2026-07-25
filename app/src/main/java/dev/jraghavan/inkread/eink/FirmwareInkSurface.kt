package dev.jraghavan.inkread.eink

import android.view.SurfaceView
import android.view.View
import java.lang.ref.WeakReference

/**
 * Process-local hand-off of the reader's panel-owning [SurfaceView] to device ink backends.
 *
 * ReaderActivity already hands its SurfaceView to [EinkAdapter.attachView]. Keeping a weak reference
 * here lets the BOOX ink backend attach to that same surface without threading vendor-specific code
 * through the 1,900-line activity. The weak reference avoids retaining a destroyed Activity/window.
 */
internal object FirmwareInkSurface {
    @Volatile private var surfaceRef: WeakReference<SurfaceView>? = null

    fun attach(view: View?) {
        surfaceRef = (view as? SurfaceView)?.let(::WeakReference)
    }

    fun current(): SurfaceView? = surfaceRef?.get()
}

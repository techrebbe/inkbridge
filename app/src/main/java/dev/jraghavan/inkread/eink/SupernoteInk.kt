package dev.jraghavan.inkread.eink

import android.content.Context
import android.os.IBinder
import android.os.Parcel
import android.util.Log

/**
 * Firmware handwriting entry point used by ReaderActivity.
 *
 * The historic class name is retained to keep the reader shell unchanged, but this object now
 * dispatches to the BOOX [TouchHelper]-backed [BooxInk] implementation when running on a BOOX device.
 * Everywhere else it preserves the original Supernote HandWriteClient Binder path below.
 *
 * Supernote path (RR19): talks to the firmware's pen daemon over `service_myservice`. The firmware
 * paints stylus ink straight to the EPDC overlay at sub-frame latency, so the app never renders the
 * live stroke itself — it only claims the pen, sets the nib, and clears the overlay.
 *
 * Clean-room (RR18): the Supernote Binder contract is reproduced from plateaukao's AGPL-3 sources
 * (supernote_draw/SupernoteInk.kt and koreader pencil.koplugin/lib/supernote_ink.lua), which
 * reimplement Ratta's HandWriteClient. Only the documented binder contract is reproduced — no
 * decompiled Ratta bytes. Vendor code stays in the Kotlin shell (IR-7).
 */
class SupernoteInk(private val context: Context) {

    private val useBoox = BooxInk.isBooxDevice()
    private var boox: BooxInk? = null

    private var cached: IBinder? = null
    private var active = false

    /** Resolve (and cache) the Supernote firmware binder via hidden ServiceManager.getService. */
    private fun binder(): IBinder? {
        cached?.let { if (it.isBinderAlive) return it }
        cached = try {
            val sm = Class.forName("android.os.ServiceManager")
            val get = sm.getMethod("getService", String::class.java)
            SERVICE_NAMES.firstNotNullOfOrNull { name -> get.invoke(null, name) as? IBinder }
        } catch (t: Throwable) {
            Log.w(TAG, "ServiceManager.getService failed (hidden-API?): ${t.javaClass.simpleName}: ${t.message}")
            null
        }
        return cached
    }

    private fun booxBackend(): BooxInk? {
        if (!useBoox) return null
        boox?.let { return it }
        val surface = FirmwareInkSurface.current() ?: run {
            Log.w(TAG, "BOOX detected but reader SurfaceView is not attached yet")
            return null
        }
        return BooxInk(
            surface,
            object : BooxInk.Listener {
                override fun onBooxPenStroke(samples: List<BooxInk.Sample>) {
                    // PR #2 proves device selection + native wet ink. The next slice routes these
                    // completed raw samples into the existing Rust-owned portable ink model.
                    Log.i(TAG, "BOOX raw pen stroke captured: ${samples.size} samples")
                }

                override fun onBooxEraserGesture(samples: List<BooxInk.Sample>) {
                    Log.i(TAG, "BOOX raw eraser gesture captured: ${samples.size} samples")
                }

                override fun onBooxInkStatus(message: String) {
                    Log.i(TAG, message)
                }
            },
        ).also { boox = it }
    }

    fun isAvailable(): Boolean = if (useBoox) {
        FirmwareInkSurface.current() != null
    } else {
        binder() != null
    }

    /** Run a Supernote transaction: interface-token + app-name preamble, then per-call ints. */
    private fun send(code: Int, write: (Parcel) -> Unit) {
        val b = binder() ?: return
        val data = Parcel.obtain()
        val reply = Parcel.obtain()
        try {
            data.writeInterfaceToken(IFACE_TOKEN)
            data.writeString(APP_NAME)
            write(data)
            b.transact(code, data, reply, 0)
        } catch (t: Throwable) {
            Log.w(TAG, "transact(code=$code) failed: ${t.javaClass.simpleName}: ${t.message}")
        } finally {
            data.recycle()
            reply.recycle()
        }
    }

    /** Supernote reflection: getSystemService("eink").enableFullUiAuto(boolean). */
    private fun enableFullUiAuto(enable: Boolean) {
        try {
            val eink = context.getSystemService("eink") ?: return
            eink.javaClass.getMethod("enableFullUiAuto", Boolean::class.javaPrimitiveType)
                .invoke(eink, enable)
        } catch (t: Throwable) {
            Log.w(TAG, "enableFullUiAuto($enable) failed: ${t.javaClass.simpleName}: ${t.message}")
        }
    }

    /** Claim the device-native pen path. Idempotent and safe to call on every focus gain. */
    fun setup(): Boolean {
        if (useBoox) return booxBackend()?.setup() == true

        if (binder() == null) return false
        send(TX_WRITE_APP_INFO) { it.writeInt(0); it.writeInt(0) }
        enableFullUiAuto(true)
        send(TX_DISABLE_AREA) { it.writeInt(0) } // no disabled areas
        send(TX_PEN) { it.writeInt(PEN_NEEDLE); it.writeInt(SIZE_EMR); it.writeInt(COLOR_BLACK) }
        active = true
        Log.i(TAG, "Supernote firmware ink claimed (pen=needle)")
        return true
    }

    /** Clear transient firmware ink before a page/tool transition. */
    fun clearAll() {
        if (useBoox) {
            boox?.clearAll()
            return
        }
        if (!active) return
        send(TX_DRAW_BUFFER) { it.writeInt(255); it.writeInt(0) }
    }

    /** Enable/disable firmware pen painting over the reader surface. */
    fun setWritable(enable: Boolean) {
        if (useBoox) {
            booxBackend()?.setWritable(enable)
            return
        }
        if (!active) return
        val edge = if (enable) WRITABLE_ON_EDGE else WRITABLE_OFF_EDGE
        send(TX_DISABLE_AREA) {
            it.writeInt(1) // one rect
            it.writeInt(0); it.writeInt(0); it.writeInt(edge); it.writeInt(edge); it.writeInt(0)
        }
    }

    /** Release the device-native ink path and clear any transient overlay. */
    fun teardown() {
        if (useBoox) {
            boox?.teardown()
            boox = null
            return
        }
        if (!active) return
        clearAll()
        enableFullUiAuto(false)
        active = false
        Log.i(TAG, "Supernote firmware ink released")
    }

    private companion object {
        const val TAG = "FirmwareInk"
        val SERVICE_NAMES = arrayOf("service_myservice", "service.myservice")
        const val IFACE_TOKEN = "android.demo.IMyService"
        const val APP_NAME = "inkread"

        const val TX_WRITE_APP_INFO = 0
        const val TX_DISABLE_AREA = 1
        const val TX_PEN = 2
        const val TX_DRAW_BUFFER = 6

        const val PEN_NEEDLE = 10
        const val COLOR_BLACK = 0
        const val SIZE_EMR = 1000

        // sendWritable sentinel rects (HandWriteClient): 18888 = ink on, 19999 = ink off.
        const val WRITABLE_ON_EDGE = 18888
        const val WRITABLE_OFF_EDGE = 19999
    }
}

package dev.exact.canvas

import android.app.Activity

/**
 * The activity a headset session runs in (docs/quest.md): an app whose GPU module opens an OpenXR session (app.json
 * `host.android.xr`) hands the runtime this activity, which the module reads over JNI (`gpu/src/openxr.rs`) as it
 * makes its device. Set as each activity is made, before the host boots, and cleared when that activity goes.
 */
object Immersive {
    @JvmField @Volatile var activity: Activity? = null
}

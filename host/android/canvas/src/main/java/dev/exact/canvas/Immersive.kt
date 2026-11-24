package dev.exact.canvas

import android.app.Activity

/**
 * The activity a headset session runs in (docs/quest.md): a headset app's OpenXR artifact (app.json
 * `host.android.xr`) hands the runtime this activity, which it reads over JNI (`gpu/openxr/src/java.rs`) as the GPU
 * module makes its device. Set as each activity is made, before the host boots, and cleared when that activity goes.
 */
object Immersive {
    @JvmField @Volatile var activity: Activity? = null

    /** Whether this app is a headset app: the manifest's `dev.exact.xr` meta-data, which `host.android.xr` adds. */
    fun headset(activity: Activity): Boolean = try {
        val info = activity.packageManager.getApplicationInfo(activity.packageName, android.content.pm.PackageManager.GET_META_DATA)
        info.metaData?.getString("dev.exact.xr") != null
    } catch (_: Exception) {
        false
    }
}

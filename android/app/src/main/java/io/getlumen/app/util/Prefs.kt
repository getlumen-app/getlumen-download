package io.getlumen.app.util

import android.content.Context
import android.content.SharedPreferences

object Prefs {
    private const val NAME = "lumen"
    private const val KEY_PROTEUS_KEY = "proteus_key"

    @Volatile
    private var prefs: SharedPreferences? = null

    fun init(context: Context) {
        if (prefs == null) {
            synchronized(this) {
                if (prefs == null) {
                    prefs = context.applicationContext.getSharedPreferences(NAME, Context.MODE_PRIVATE)
                }
            }
        }
    }

    private fun p(): SharedPreferences =
        prefs ?: throw IllegalStateException("Prefs.init() not called")

    var proteusKey: String
        get() = p().getString(KEY_PROTEUS_KEY, "") ?: ""
        set(value) = p().edit().putString(KEY_PROTEUS_KEY, value.trim()).apply()

    /** Last VPN state as seen by the service; the Activity reads it on resume. */
    var lastStatus: String
        get() = p().getString("last_status", "idle") ?: "idle"
        set(value) = p().edit().putString("last_status", value).apply()
}

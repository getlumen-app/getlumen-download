package io.getlumen.app

import android.app.Application
import io.getlumen.app.util.Prefs

class LumenApp : Application() {
    override fun onCreate() {
        super.onCreate()
        Prefs.init(this)
    }
}

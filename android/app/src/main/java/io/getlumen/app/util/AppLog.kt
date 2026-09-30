package io.getlumen.app.util

import android.util.Log
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * In-memory ring buffer for user-visible log lines. The VPN worker thread,
 * sing-box and the Telemost joiner all report here; MainActivity mirrors the
 * buffer into a TextView while it is open.
 */
object AppLog {
    private const val MAX_LINES = 400
    private const val TAG = "Lumen"

    private val lines = ArrayDeque<String>()

    @Volatile
    var listener: ((List<String>) -> Unit)? = null

    private val timeFmt = SimpleDateFormat("HH:mm:ss", Locale.US)

    @Synchronized
    fun add(message: String) {
        val masked = mask(message)
        Log.d(TAG, masked)
        lines.addLast("${timeFmt.format(Date())}  $masked")
        while (lines.size > MAX_LINES) lines.removeFirst()
        listener?.invoke(lines.toList())
    }

    @Synchronized
    fun snapshot(): List<String> = lines.toList()

    /** Never let a Proteus key or join-link token reach the log. */
    private fun mask(s: String): String {
        var out = s
        Regex("[?&]sub=[A-Za-z0-9_-]{6,}").find(out)?.let { m ->
            out = out.replace(m.value, m.value.take(5) + "***")
        }
        Regex("telemost\\.yandex\\.ru/j/[A-Za-z0-9_-]+").find(out)?.let { m ->
            out = out.replace(m.value, "telemost.yandex.ru/j/***")
        }
        return out
    }
}

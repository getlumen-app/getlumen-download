package io.getlumen.app.util

/**
 * Base64 that works both on-device (android.util.Base64) and in local JVM
 * unit tests (java.util.Base64, API 26+). java.util is preferred when
 * present; the android.util stub throws on the JVM.
 */
object B64 {
    private val jvmDecoder: java.util.Base64.Decoder? = runCatching {
        java.util.Base64.getDecoder()
    }.getOrNull()

    fun decode(s: String): ByteArray =
        jvmDecoder?.decode(s) ?: android.util.Base64.decode(s, android.util.Base64.DEFAULT)
}
